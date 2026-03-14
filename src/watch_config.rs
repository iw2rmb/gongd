use std::{
    io,
    path::{Path, PathBuf},
};

use notify::{Config, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use tokio::sync::mpsc;

use crate::{
    config::{ConfigStore, GongdConfig},
    repo::{build_startup_repos, RepoState},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredRepo {
    pub original: PathBuf,
    pub resolved: PathBuf,
}

struct LoadedConfiguredRepos {
    repos: Vec<ConfiguredRepo>,
    changed: bool,
}

pub struct ConfigWatch {
    startup_cli_inputs: Vec<PathBuf>,
    store: ConfigStore,
    rx: Option<mpsc::UnboundedReceiver<()>>,
    watcher: Option<RecommendedWatcher>,
}

impl ConfigWatch {
    pub fn new(store: ConfigStore, startup_cli_inputs: Vec<PathBuf>) -> Self {
        Self {
            startup_cli_inputs,
            store,
            rx: None,
            watcher: None,
        }
    }

    pub fn start(&mut self) -> io::Result<()> {
        if self.watcher.is_some() {
            return Ok(());
        }

        let watch_dir = self.store.watch_dir();
        std::fs::create_dir_all(&watch_dir)?;

        let (tx, rx) = mpsc::unbounded_channel();
        let watcher = start_config_watcher(&watch_dir, tx)
            .map_err(|err| io::Error::other(err.to_string()))?;

        self.rx = Some(rx);
        self.watcher = Some(watcher);
        Ok(())
    }

    pub async fn recv(&mut self) -> Option<()> {
        match self.rx.as_mut() {
            Some(rx) => rx.recv().await,
            None => None,
        }
    }

    pub fn seed_from_cli_if_needed(&self) -> io::Result<()> {
        if self.startup_cli_inputs.is_empty() {
            return Ok(());
        }

        let config_exists = self.store.exists();
        let config = match self.store.load() {
            Ok(config) => config,
            Err(err) if err.kind() == io::ErrorKind::InvalidData => return Ok(()),
            Err(err) => return Err(err),
        };

        if config_exists && !config.repos.is_empty() {
            return Ok(());
        }

        let repos = load_configured_repos(self.startup_cli_inputs.clone());
        if repos.repos.is_empty() {
            return Ok(());
        }

        self.save_configured_repos(&repos.repos)
    }

    pub fn load_repo_states_for_apply(&self) -> io::Result<Option<Vec<RepoState>>> {
        let loaded = match self.load_configured_repos_snapshot() {
            Ok(loaded) => loaded,
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                eprintln!("{err}");
                return Ok(None);
            }
            Err(err) => return Err(err),
        };

        if loaded.changed {
            self.save_configured_repos(&loaded.repos)?;
        }

        Ok(Some(build_startup_repos(
            loaded.repos.into_iter().map(|repo| repo.resolved),
        )))
    }

    pub fn load_configured_repos_for_write(&self) -> io::Result<Vec<ConfiguredRepo>> {
        let loaded = self.load_configured_repos_snapshot()?;
        if loaded.changed {
            self.save_configured_repos(&loaded.repos)?;
        }
        Ok(loaded.repos)
    }

    pub fn save_configured_repos(&self, repos: &[ConfiguredRepo]) -> io::Result<()> {
        self.store.save(&GongdConfig {
            repos: repos.iter().map(|repo| repo.original.clone()).collect(),
        })
    }

    fn load_configured_repos_snapshot(&self) -> io::Result<LoadedConfiguredRepos> {
        let config = self.store.load()?;
        Ok(load_configured_repos(config.repos))
    }
}

fn start_config_watcher(
    watch_dir: &Path,
    tx: mpsc::UnboundedSender<()>,
) -> NotifyResult<RecommendedWatcher> {
    let mut watcher = RecommendedWatcher::new(
        move |event| match event {
            Ok(_) => {
                let _ = tx.send(());
            }
            Err(err) => eprintln!("config watch error: {err}"),
        },
        Config::default(),
    )?;

    watcher.watch(watch_dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

impl ConfiguredRepo {
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let resolved = RepoState::discover(path)?.root;
        Ok(Self {
            original: path.to_path_buf(),
            resolved,
        })
    }
}

fn load_configured_repos(paths: Vec<PathBuf>) -> LoadedConfiguredRepos {
    let mut repos = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for original in &paths {
        match ConfiguredRepo::from_path(original) {
            Ok(repo) if !seen.insert(repo.resolved.clone()) => {}
            Ok(repo) => repos.push(repo),
            Err(err) => eprintln!("skipping {}: {err}", original.display()),
        }
    }

    let changed = repos
        .iter()
        .map(|repo| repo.original.clone())
        .collect::<Vec<_>>()
        != paths;
    LoadedConfiguredRepos { repos, changed }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use tokio::sync::{mpsc, RwLock};

    use super::ConfigWatch;
    use crate::{
        config::{ConfigStore, GongdConfig},
        test_support::{env_lock, init_git_repo, ScopedEnvVar, TestDir},
        watch::WatchManager,
    };

    #[tokio::test]
    async fn config_reload_dedupes_by_resolved_path_and_keeps_first_original() {
        let _guard = env_lock().lock().unwrap();
        let home = TestDir::new("gongd-config-home");
        let repo = home.path().join("repo");
        init_git_repo(&repo);
        let repo_root = std::fs::canonicalize(&repo).unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());

        let store = ConfigStore::new(home.path().join(".gong").join("config.json"));
        store
            .save(&GongdConfig {
                repos: vec![PathBuf::from("~/repo"), repo_root.clone()],
            })
            .unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let repos = Arc::new(RwLock::new(Vec::new()));
        let mut manager = WatchManager::new(repos.clone(), raw_tx, Vec::new(), store.clone());

        manager.initialize().await.unwrap();

        assert_eq!(store.load().unwrap().repos, vec![PathBuf::from("~/repo")]);
        assert_eq!(
            repos
                .read()
                .await
                .iter()
                .map(|repo| repo.root.clone())
                .collect::<Vec<_>>(),
            vec![repo_root]
        );
    }

    #[test]
    fn seed_from_cli_keeps_original_paths() {
        let _guard = env_lock().lock().unwrap();
        let home = TestDir::new("gongd-config-seed-home");
        let repo = home.path().join("repo");
        init_git_repo(&repo);
        let _home = ScopedEnvVar::set("HOME", home.path());

        let store = ConfigStore::new(home.path().join(".gong").join("config.json"));
        let config_watch = ConfigWatch::new(store.clone(), vec![PathBuf::from("~/repo")]);

        config_watch.seed_from_cli_if_needed().unwrap();

        assert_eq!(store.load().unwrap().repos, vec![PathBuf::from("~/repo")]);
    }
}
