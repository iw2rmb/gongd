use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::{
    config::ConfigStore,
    protocol::ControlResponse,
    repo::{normalize_repo_root, RepoState},
    watch_config::ConfigWatch,
};

pub type RawEvent = notify::Result<Event>;
pub type SharedRepos = std::sync::Arc<RwLock<Vec<RepoState>>>;

pub enum ManagerRequest {
    AddWatch {
        repo: PathBuf,
        respond_to: oneshot::Sender<ControlResponse>,
    },
    RemoveWatch {
        repo: PathBuf,
        respond_to: oneshot::Sender<ControlResponse>,
    },
    ListWatches {
        respond_to: oneshot::Sender<ControlResponse>,
    },
}

struct WatchRegistration {
    state: RepoState,
    _watcher: RecommendedWatcher,
}

pub struct WatchManager {
    watchers: BTreeMap<PathBuf, WatchRegistration>,
    repos: SharedRepos,
    raw_tx: mpsc::Sender<RawEvent>,
    config_watch: ConfigWatch,
}

impl WatchManager {
    pub fn new(
        repos: SharedRepos,
        raw_tx: mpsc::Sender<RawEvent>,
        startup_cli_inputs: Vec<PathBuf>,
        config_store: ConfigStore,
    ) -> Self {
        let config_watch = ConfigWatch::new(config_store, startup_cli_inputs);
        Self {
            watchers: BTreeMap::new(),
            repos,
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
            ManagerRequest::AddWatch { repo, respond_to } => {
                let _ = respond_to.send(self.add_watch(&repo));
            }
            ManagerRequest::RemoveWatch { repo, respond_to } => {
                let _ = respond_to.send(self.remove_watch(&repo));
            }
            ManagerRequest::ListWatches { respond_to } => {
                let _ = respond_to.send(self.list_watches());
            }
        }
    }

    fn add_watch(&mut self, repo: &Path) -> ControlResponse {
        self.try_add_watch(repo)
            .unwrap_or_else(|err| ControlResponse::error(err.to_string()))
    }

    fn remove_watch(&mut self, repo: &Path) -> ControlResponse {
        self.try_remove_watch(repo)
            .unwrap_or_else(|err| ControlResponse::error(err.to_string()))
    }

    fn list_watches(&self) -> ControlResponse {
        ControlResponse::list(self.watchers.keys().cloned().collect())
    }

    fn try_add_watch(&self, repo: &Path) -> io::Result<ControlResponse> {
        let root = RepoState::discover(repo)?.root;
        let inserted = self.persist_roots(|roots| Ok(roots.insert(root.clone())))?;

        Ok(if inserted {
            ControlResponse::success_message(format!("watch added for {}", root.display()))
        } else {
            ControlResponse::success_message(format!("watch already configured for {}", root.display()))
        })
    }

    fn try_remove_watch(&self, repo: &Path) -> io::Result<ControlResponse> {
        let root = normalize_repo_root(repo)?;
        self.persist_roots(|roots| {
            if roots.remove(&root) {
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

    fn persist_roots<T>(
        &self,
        mutate: impl FnOnce(&mut BTreeSet<PathBuf>) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut roots = self.config_watch.load_roots_for_write()?;
        let result = mutate(&mut roots)?;
        self.config_watch.save_roots(&roots)?;
        Ok(result)
    }

    async fn reload_config_from_disk(&mut self) -> io::Result<()> {
        let Some(repos) = self.config_watch.load_repo_states_for_apply()? else {
            return Ok(());
        };

        self.reconcile_watches(repos).await;
        Ok(())
    }

    async fn reconcile_watches(&mut self, desired_repos: Vec<RepoState>) {
        let desired_roots: BTreeSet<PathBuf> =
            desired_repos.iter().map(|repo| repo.root.clone()).collect();

        self.watchers
            .retain(|root, _| desired_roots.contains(root));

        for repo in desired_repos {
            if let Err(err) = self.ensure_active(repo.clone()) {
                eprintln!("failed to watch {}: {err}", repo.root.display());
            }
        }

        self.sync_shared_repos().await;
    }

    fn ensure_active(&mut self, repo: RepoState) -> NotifyResult<()> {
        if self.watchers.contains_key(&repo.root) {
            return Ok(());
        }

        let watcher = start_repo_watcher(&repo, self.raw_tx.clone())?;
        self.watchers.insert(
            repo.root.clone(),
            WatchRegistration {
                state: repo,
                _watcher: watcher,
            },
        );
        Ok(())
    }

    async fn sync_shared_repos(&self) {
        let mut repos = self.repos.write().await;
        *repos = self
            .watchers
            .values()
            .map(|registration| registration.state.clone())
            .collect();
        repos.sort_by(|left, right| left.root.cmp(&right.root));
    }
}

fn start_repo_watcher(repo: &RepoState, tx: mpsc::Sender<RawEvent>) -> NotifyResult<RecommendedWatcher> {
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = tx.blocking_send(event);
        },
        Config::default(),
    )?;

    watcher.watch(&repo.root, RecursiveMode::Recursive)?;
    watcher.watch(&repo.git_dir, RecursiveMode::Recursive)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::{mpsc, oneshot, RwLock};

    use super::{ManagerRequest, WatchManager};
    use crate::{
        config::ConfigStore,
        test_support::{init_git_repo, wait_for, TestDir},
    };

    #[tokio::test]
    async fn startup_seed_and_control_requests_flow_through_config_reload() {
        let tmp = TestDir::new("gongd-watch-manager");
        let cli_repo = tmp.path().join("cli");
        let added_repo = tmp.path().join("added");
        init_git_repo(&cli_repo);
        init_git_repo(&added_repo);
        let cli_root = std::fs::canonicalize(&cli_repo).unwrap();
        let added_root = std::fs::canonicalize(&added_repo).unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let repos = Arc::new(RwLock::new(Vec::new()));
        let store = ConfigStore::new(tmp.path().join(".gong").join("config.json"));

        let mut manager = WatchManager::new(repos.clone(), raw_tx, vec![cli_repo.clone()], store.clone());
        manager.initialize().await.unwrap();

        let (manager_tx, manager_rx) = mpsc::channel(16);
        let manager_handle = tokio::spawn(manager.run(manager_rx));

        wait_for(|| store.load().ok().map(|config| config.repos) == Some(vec![cli_root.clone()]))
            .await;
        wait_for(|| {
            repos.try_read()
                .map(|repos| repos.iter().any(|repo| repo.root == cli_root))
                .unwrap_or(false)
        })
        .await;

        let add_response = send_add_watch(&manager_tx, added_repo.clone()).await;
        assert!(add_response.ok);

        wait_for(|| {
            store.load().ok().map(|config| config.repos)
                == Some(vec![added_root.clone(), cli_root.clone()])
        })
        .await;
        wait_for(|| {
            repos.try_read()
                .map(|repos| repos.iter().any(|repo| repo.root == added_root))
                .unwrap_or(false)
        })
        .await;

        let remove_response = send_remove_watch(&manager_tx, cli_repo.clone()).await;
        assert!(remove_response.ok);

        wait_for(|| store.load().ok().map(|config| config.repos) == Some(vec![added_root.clone()]))
            .await;
        wait_for(|| {
            repos.try_read()
                .map(|repos| repos.iter().all(|repo| repo.root != cli_root))
                .unwrap_or(false)
        })
        .await;

        manager_handle.abort();
    }

    async fn send_add_watch(
        tx: &mpsc::Sender<ManagerRequest>,
        repo: std::path::PathBuf,
    ) -> crate::protocol::ControlResponse {
        let (respond_to, response_rx) = oneshot::channel();
        let request = ManagerRequest::AddWatch { repo, respond_to };

        tx.send(request).await.unwrap();
        response_rx.await.unwrap()
    }

    async fn send_remove_watch(
        tx: &mpsc::Sender<ManagerRequest>,
        repo: std::path::PathBuf,
    ) -> crate::protocol::ControlResponse {
        let (respond_to, response_rx) = oneshot::channel();
        let request = ManagerRequest::RemoveWatch { repo, respond_to };

        tx.send(request).await.unwrap();
        response_rx.await.unwrap()
    }
}
