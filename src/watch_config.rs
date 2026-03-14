use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use notify::{Config, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use tokio::sync::mpsc;

use crate::{
    config::{ConfigStore, GongdConfig},
    repo::{build_startup_repos, normalize_repo_root, RepoState},
};

struct LoadedRoots {
    roots: BTreeSet<PathBuf>,
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

        let roots = discover_repo_roots(self.startup_cli_inputs.clone());
        if roots.is_empty() {
            return Ok(());
        }

        self.save_roots(&roots)
    }

    pub fn load_repo_states_for_apply(&self) -> io::Result<Option<Vec<RepoState>>> {
        let loaded = match self.load_roots_snapshot() {
            Ok(loaded) => loaded,
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                eprintln!("{err}");
                return Ok(None);
            }
            Err(err) => return Err(err),
        };

        if loaded.changed {
            self.save_roots(&loaded.roots)?;
        }

        Ok(Some(build_startup_repos(loaded.roots)))
    }

    pub fn load_roots_for_write(&self) -> io::Result<BTreeSet<PathBuf>> {
        let config = self.store.load()?;
        Ok(load_roots_for_write(config.repos))
    }

    pub fn save_roots(&self, roots: &BTreeSet<PathBuf>) -> io::Result<()> {
        self.store.save(&GongdConfig {
            repos: roots.iter().cloned().collect(),
        })
    }

    fn load_roots_snapshot(&self) -> io::Result<LoadedRoots> {
        let config = self.store.load()?;
        let roots = discover_repo_roots(config.repos.clone());
        let changed = roots.iter().cloned().collect::<Vec<_>>() != config.repos;
        Ok(LoadedRoots { roots, changed })
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

fn discover_repo_roots(paths: Vec<PathBuf>) -> BTreeSet<PathBuf> {
    build_startup_repos(paths)
        .into_iter()
        .map(|repo| repo.root)
        .collect()
}

fn load_roots_for_write(paths: Vec<PathBuf>) -> BTreeSet<PathBuf> {
    paths
        .into_iter()
        .filter_map(|path| normalize_repo_root(&path).ok())
        .collect()
}
