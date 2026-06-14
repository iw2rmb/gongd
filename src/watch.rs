use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::{
    config::ConfigStore,
    folder::{normalize_folder_root, MonitoredFolder},
    protocol::ControlResponse,
    watch_config::{ConfigWatch, ConfiguredFolder},
};

pub type RawEvent = notify::Result<Event>;
pub type SharedFolders = std::sync::Arc<RwLock<Vec<MonitoredFolder>>>;

pub enum ManagerRequest {
    AddWatch {
        folder: PathBuf,
        respond_to: oneshot::Sender<ControlResponse>,
    },
    RemoveWatch {
        folder: PathBuf,
        respond_to: oneshot::Sender<ControlResponse>,
    },
    ListWatches {
        respond_to: oneshot::Sender<ControlResponse>,
    },
}

struct WatchRegistration {
    state: MonitoredFolder,
    _watcher: RecommendedWatcher,
}

pub struct WatchManager {
    watchers: BTreeMap<PathBuf, WatchRegistration>,
    folders: SharedFolders,
    raw_tx: mpsc::Sender<RawEvent>,
    config_watch: ConfigWatch,
}

impl WatchManager {
    pub fn new(
        folders: SharedFolders,
        raw_tx: mpsc::Sender<RawEvent>,
        startup_cli_inputs: Vec<PathBuf>,
        config_store: ConfigStore,
    ) -> Self {
        let config_watch = ConfigWatch::new(config_store, startup_cli_inputs);
        Self {
            watchers: BTreeMap::new(),
            folders,
            raw_tx,
            config_watch,
        }
    }

    pub async fn initialize(&mut self) -> io::Result<()> {
        self.config_watch.start()?;
        self.reload_config_from_disk().await?;
        self.config_watch.seed_from_cli_if_needed()?;
        Ok(())
    }

    pub async fn run(mut self, mut rx: mpsc::Receiver<ManagerRequest>) {
        loop {
            tokio::select! {
                Some(request) = rx.recv() => self.handle_request(request),
                Some(()) = self.config_watch.recv() => {
                    if let Err(err) = self.reload_config_from_disk().await {
                        eprintln!("config reload failed: {err}");
                    }
                }
                else => return,
            }
        }
    }

    fn handle_request(&mut self, request: ManagerRequest) {
        match request {
            ManagerRequest::AddWatch { folder, respond_to } => {
                let _ = respond_to.send(self.add_watch(&folder));
            }
            ManagerRequest::RemoveWatch { folder, respond_to } => {
                let _ = respond_to.send(self.remove_watch(&folder));
            }
            ManagerRequest::ListWatches { respond_to } => {
                let _ = respond_to.send(self.list_watches());
            }
        }
    }

    fn add_watch(&mut self, folder: &Path) -> ControlResponse {
        respond(self.try_add_watch(folder))
    }

    fn remove_watch(&mut self, folder: &Path) -> ControlResponse {
        respond(self.try_remove_watch(folder))
    }

    fn list_watches(&self) -> ControlResponse {
        ControlResponse::list(self.watchers.keys().cloned().collect())
    }

    fn try_add_watch(&self, folder: &Path) -> io::Result<ControlResponse> {
        let folder = ConfiguredFolder::from_path(folder)?;
        let inserted = self.persist_folders(|folders| {
            if folders
                .iter()
                .any(|existing| existing.resolved() == folder.resolved())
            {
                Ok(false)
            } else {
                folders.push(folder.clone());
                Ok(true)
            }
        })?;

        Ok(if inserted {
            ControlResponse::success_message(format!(
                "watch added for {}",
                folder.resolved().display()
            ))
        } else {
            ControlResponse::success_message(format!(
                "watch already configured for {}",
                folder.resolved().display()
            ))
        })
    }

    fn try_remove_watch(&self, folder: &Path) -> io::Result<ControlResponse> {
        let root = normalize_folder_root(folder)?;
        self.persist_folders(|folders| {
            if let Some(index) = folders.iter().position(|folder| folder.resolved() == root) {
                folders.remove(index);
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("watch not found for {}", root.display()),
                ))
            }
        })?;

        Ok(ControlResponse::success_message(format!(
            "watch removed for {}",
            root.display()
        )))
    }

    fn persist_folders<T>(
        &self,
        mutate: impl FnOnce(&mut Vec<ConfiguredFolder>) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut folders = self.config_watch.load_configured_folders_for_write()?;
        let result = mutate(&mut folders)?;
        self.config_watch.save_configured_folders(&folders)?;
        Ok(result)
    }

    async fn reload_config_from_disk(&mut self) -> io::Result<()> {
        let Some(folders) = self.config_watch.load_folder_states_for_apply()? else {
            return Ok(());
        };

        self.reconcile_watches(folders).await;
        Ok(())
    }

    async fn reconcile_watches(&mut self, desired_folders: Vec<MonitoredFolder>) {
        let desired_roots: BTreeSet<PathBuf> = desired_folders
            .iter()
            .map(|folder| folder.root.clone())
            .collect();

        self.watchers.retain(|root, _| desired_roots.contains(root));

        for folder in desired_folders {
            if let Err(err) = self.ensure_active(folder.clone()) {
                eprintln!("failed to watch {}: {err}", folder.root.display());
            }
        }

        self.sync_shared_folders().await;
    }

    fn ensure_active(&mut self, folder: MonitoredFolder) -> NotifyResult<()> {
        if self.watchers.contains_key(&folder.root) {
            return Ok(());
        }

        let watcher = start_folder_watcher(&folder, self.raw_tx.clone())?;
        self.watchers.insert(
            folder.root.clone(),
            WatchRegistration {
                state: folder,
                _watcher: watcher,
            },
        );
        Ok(())
    }

    async fn sync_shared_folders(&self) {
        let mut folders = self.folders.write().await;
        *folders = self
            .watchers
            .values()
            .map(|registration| registration.state.clone())
            .collect();
        folders.sort_by(|left, right| left.root.cmp(&right.root));
    }
}

fn respond(result: io::Result<ControlResponse>) -> ControlResponse {
    result.unwrap_or_else(|err| ControlResponse::error(err.to_string()))
}

fn start_folder_watcher(
    folder: &MonitoredFolder,
    tx: mpsc::Sender<RawEvent>,
) -> NotifyResult<RecommendedWatcher> {
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = tx.blocking_send(event);
        },
        Config::default(),
    )?;

    watcher.watch(&folder.root, RecursiveMode::Recursive)?;
    if let Some(git_dir) = folder.git_dir() {
        watcher.watch(git_dir, RecursiveMode::Recursive)?;
    }
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::Arc};

    use tokio::sync::{mpsc, oneshot, RwLock};

    use super::{ManagerRequest, WatchManager};
    use crate::{
        config::{ConfigStore, GongdConfig},
        test_support::{env_lock, wait_for, ScopedEnvVar, TestDir},
    };

    #[tokio::test]
    async fn startup_seed_and_control_requests_flow_through_config_reload() {
        let tmp = TestDir::new("gongd-watch-manager");
        let cli_folder = tmp.path().join("cli");
        let added_folder = tmp.path().join("added");
        fs::create_dir_all(&cli_folder).unwrap();
        fs::create_dir_all(&added_folder).unwrap();
        let cli_root = std::fs::canonicalize(&cli_folder).unwrap();
        let added_root = std::fs::canonicalize(&added_folder).unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let folders = Arc::new(RwLock::new(Vec::new()));
        let store = ConfigStore::new(tmp.path().join(".gong").join("config.json"));

        let mut manager = WatchManager::new(
            folders.clone(),
            raw_tx,
            vec![cli_folder.clone()],
            store.clone(),
        );
        manager.initialize().await.unwrap();

        let (manager_tx, manager_rx) = mpsc::channel(16);
        let manager_handle = tokio::spawn(manager.run(manager_rx));

        wait_for(|| {
            store.load().ok().map(|config| config.folders) == Some(vec![cli_folder.clone()])
        })
        .await;
        wait_for(|| {
            folders
                .try_read()
                .map(|folders| folders.iter().any(|folder| folder.root == cli_root))
                .unwrap_or(false)
        })
        .await;

        let add_response = send_watch_request(&manager_tx, |respond_to| ManagerRequest::AddWatch {
            folder: added_folder.clone(),
            respond_to,
        })
        .await;
        assert!(add_response.ok);

        wait_for(|| {
            store.load().ok().map(|config| config.folders)
                == Some(vec![cli_folder.clone(), added_folder.clone()])
        })
        .await;
        wait_for(|| {
            folders
                .try_read()
                .map(|folders| folders.iter().any(|folder| folder.root == added_root))
                .unwrap_or(false)
        })
        .await;

        let remove_response =
            send_watch_request(&manager_tx, |respond_to| ManagerRequest::RemoveWatch {
                folder: cli_folder.clone(),
                respond_to,
            })
            .await;
        assert!(remove_response.ok);

        wait_for(|| {
            store.load().ok().map(|config| config.folders) == Some(vec![added_folder.clone()])
        })
        .await;
        wait_for(|| {
            folders
                .try_read()
                .map(|folders| folders.iter().all(|folder| folder.root != cli_root))
                .unwrap_or(false)
        })
        .await;

        manager_handle.abort();
    }

    #[tokio::test]
    async fn initialize_prunes_missing_folders_from_config() {
        let tmp = TestDir::new("gongd-watch-manager-prune");
        let present_folder = tmp.path().join("present");
        let missing_folder = tmp.path().join("missing");
        fs::create_dir_all(&present_folder).unwrap();
        let present_root = std::fs::canonicalize(&present_folder).unwrap();

        let store = ConfigStore::new(tmp.path().join(".gong").join("config.json"));
        store
            .save(&GongdConfig {
                folders: vec![missing_folder, present_folder.clone()],
            })
            .unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let folders = Arc::new(RwLock::new(Vec::new()));
        let mut manager = WatchManager::new(folders.clone(), raw_tx, Vec::new(), store.clone());

        manager.initialize().await.unwrap();

        assert_eq!(store.load().unwrap().folders, vec![present_folder.clone()]);
        assert_eq!(
            folders
                .read()
                .await
                .iter()
                .map(|folder| folder.root.clone())
                .collect::<Vec<_>>(),
            vec![present_root]
        );
    }

    #[tokio::test]
    async fn add_watch_dedupes_against_existing_config_by_resolved_path() {
        let _guard = env_lock().lock().await;
        let home = TestDir::new("gongd-watch-manager-home");
        let folder = home.path().join("folder");
        fs::create_dir_all(&folder).unwrap();
        let folder_root = std::fs::canonicalize(&folder).unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());

        let store = ConfigStore::new(home.path().join(".gong").join("config.json"));
        store
            .save(&GongdConfig {
                folders: vec![PathBuf::from("~/folder")],
            })
            .unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let folders = Arc::new(RwLock::new(Vec::new()));
        let mut manager = WatchManager::new(folders, raw_tx, Vec::new(), store.clone());
        manager.initialize().await.unwrap();

        let response = manager.try_add_watch(&folder_root).unwrap();
        let expected_message = format!("watch already configured for {}", folder_root.display());

        assert!(response.ok);
        assert_eq!(response.message.as_deref(), Some(expected_message.as_str()));
        assert_eq!(
            store.load().unwrap().folders,
            vec![PathBuf::from("~/folder")]
        );
    }

    #[tokio::test]
    async fn remove_watch_matches_configured_folder_by_resolved_path() {
        let _guard = env_lock().lock().await;
        let home = TestDir::new("gongd-watch-manager-remove-home");
        let folder = home.path().join("folder");
        fs::create_dir_all(&folder).unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());

        let store = ConfigStore::new(home.path().join(".gong").join("config.json"));
        store
            .save(&GongdConfig {
                folders: vec![PathBuf::from("~/folder")],
            })
            .unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let folders = Arc::new(RwLock::new(Vec::new()));
        let mut manager = WatchManager::new(folders, raw_tx, Vec::new(), store.clone());
        manager.initialize().await.unwrap();

        let response = manager.try_remove_watch(&folder).unwrap();

        assert!(response.ok);
        assert_eq!(store.load().unwrap().folders, Vec::<PathBuf>::new());
    }

    async fn send_watch_request(
        tx: &mpsc::Sender<ManagerRequest>,
        make: impl FnOnce(oneshot::Sender<crate::protocol::ControlResponse>) -> ManagerRequest,
    ) -> crate::protocol::ControlResponse {
        let (respond_to, response_rx) = oneshot::channel();

        tx.send(make(respond_to)).await.unwrap();
        response_rx.await.unwrap()
    }
}
