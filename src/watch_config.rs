use std::{
    io,
    path::{Path, PathBuf},
};

use notify::{Config, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use tokio::sync::mpsc;

use crate::{
    config::{ConfigStore, GongdConfig},
    folder::{resolve_existing_dir, MonitoredFolder},
};

#[derive(Debug, Clone)]
pub struct ConfiguredFolder {
    pub original: PathBuf,
    pub root: PathBuf,
}

struct LoadedConfiguredFolders {
    folders: Vec<ConfiguredFolder>,
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

        if config_exists && !config.folders.is_empty() {
            return Ok(());
        }

        let folders = load_configured_folders(self.startup_cli_inputs.clone());
        if folders.folders.is_empty() {
            return Ok(());
        }

        self.save_configured_folders(&folders.folders)
    }

    pub fn load_folder_states_for_apply(&self) -> io::Result<Option<Vec<MonitoredFolder>>> {
        let folders = match self.reconcile_snapshot() {
            Ok(folders) => folders,
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                eprintln!("{err}");
                return Ok(None);
            }
            Err(err) => return Err(err),
        };

        Ok(Some(
            folders
                .iter()
                .filter_map(|folder| {
                    MonitoredFolder::discover(&folder.original)
                        .map_err(|err| eprintln!("skipping {}: {err}", folder.original.display()))
                        .ok()
                })
                .collect(),
        ))
    }

    pub fn load_configured_folders_for_write(&self) -> io::Result<Vec<ConfiguredFolder>> {
        self.reconcile_snapshot()
    }

    fn reconcile_snapshot(&self) -> io::Result<Vec<ConfiguredFolder>> {
        let loaded = self.load_configured_folders_snapshot()?;
        if loaded.changed {
            self.save_configured_folders(&loaded.folders)?;
        }
        Ok(loaded.folders)
    }

    pub fn save_configured_folders(&self, folders: &[ConfiguredFolder]) -> io::Result<()> {
        self.store.save(&GongdConfig {
            folders: folders
                .iter()
                .map(|folder| folder.original.clone())
                .collect(),
        })
    }

    fn load_configured_folders_snapshot(&self) -> io::Result<LoadedConfiguredFolders> {
        let config = self.store.load()?;
        Ok(load_configured_folders(config.folders))
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

impl ConfiguredFolder {
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let root = resolve_existing_dir(path)?;
        Ok(Self {
            original: path.to_path_buf(),
            root,
        })
    }

    pub fn resolved(&self) -> &Path {
        &self.root
    }
}

fn load_configured_folders(paths: Vec<PathBuf>) -> LoadedConfiguredFolders {
    let mut folders = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for original in &paths {
        match ConfiguredFolder::from_path(original) {
            Ok(folder) if !seen.insert(folder.root.clone()) => {}
            Ok(folder) => folders.push(folder),
            Err(err) => eprintln!("skipping {}: {err}", original.display()),
        }
    }

    let changed = folders
        .iter()
        .map(|folder| folder.original.clone())
        .collect::<Vec<_>>()
        != paths;
    LoadedConfiguredFolders { folders, changed }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use tokio::sync::{mpsc, RwLock};

    use super::ConfigWatch;
    use crate::{
        config::{ConfigStore, GongdConfig},
        test_support::{env_lock, ScopedEnvVar, TestDir},
        watch::WatchManager,
    };

    #[tokio::test]
    async fn config_reload_dedupes_by_resolved_path_and_keeps_first_original() {
        let _guard = env_lock().lock().await;
        let home = TestDir::new("gongd-config-home");
        let folder = home.path().join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        let folder_root = std::fs::canonicalize(&folder).unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());

        let store = ConfigStore::new(home.path().join(".gong").join("config.json"));
        store
            .save(&GongdConfig {
                folders: vec![PathBuf::from("~/folder"), folder_root.clone()],
            })
            .unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let folders = Arc::new(RwLock::new(Vec::new()));
        let mut manager = WatchManager::new(folders.clone(), raw_tx, Vec::new(), store.clone());

        manager.initialize().await.unwrap();

        assert_eq!(
            store.load().unwrap().folders,
            vec![PathBuf::from("~/folder")]
        );
        assert_eq!(
            folders
                .read()
                .await
                .iter()
                .map(|folder| folder.root.clone())
                .collect::<Vec<_>>(),
            vec![folder_root]
        );
    }

    #[test]
    fn seed_from_cli_keeps_original_paths() {
        let _guard = env_lock().blocking_lock();
        let home = TestDir::new("gongd-config-seed-home");
        let folder = home.path().join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());

        let store = ConfigStore::new(home.path().join(".gong").join("config.json"));
        let config_watch = ConfigWatch::new(store.clone(), vec![PathBuf::from("~/folder")]);

        config_watch.seed_from_cli_if_needed().unwrap();

        assert_eq!(
            store.load().unwrap().folders,
            vec![PathBuf::from("~/folder")]
        );
    }
}
