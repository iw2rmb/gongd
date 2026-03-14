use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::PathBuf,
};

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher};
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::{
    config::{ConfigStore, GongdConfig},
    protocol::ControlResponse,
    repo::{build_startup_repos, normalize_repo_root, RepoState},
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
    cli_roots: BTreeSet<PathBuf>,
    persisted_roots: BTreeSet<PathBuf>,
    suppressed_roots: BTreeSet<PathBuf>,
    startup_cli_inputs: Vec<PathBuf>,
    startup_config_inputs: Vec<PathBuf>,
    repos: SharedRepos,
    raw_tx: mpsc::Sender<RawEvent>,
    config_store: ConfigStore,
}

impl WatchManager {
    pub fn new(
        repos: SharedRepos,
        raw_tx: mpsc::Sender<RawEvent>,
        startup_cli_inputs: Vec<PathBuf>,
        startup_config_inputs: Vec<PathBuf>,
        config_store: ConfigStore,
    ) -> Self {
        Self {
            watchers: BTreeMap::new(),
            cli_roots: BTreeSet::new(),
            persisted_roots: BTreeSet::new(),
            suppressed_roots: BTreeSet::new(),
            startup_cli_inputs,
            startup_config_inputs,
            repos,
            raw_tx,
            config_store,
        }
    }

    pub async fn initialize(&mut self) -> io::Result<()> {
        for repo in build_startup_repos(self.startup_config_inputs.clone()) {
            self.persisted_roots.insert(repo.root.clone());
            self.ensure_active(repo)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
        }

        for repo in build_startup_repos(self.startup_cli_inputs.clone()) {
            self.cli_roots.insert(repo.root.clone());
            self.persisted_roots.insert(repo.root.clone());
            self.ensure_active(repo)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
        }

        self.persist_repo_set(&self.persisted_roots)?;
        self.sync_shared_repos().await;
        Ok(())
    }

    pub async fn run(mut self, mut rx: mpsc::Receiver<ManagerRequest>) {
        while let Some(request) = rx.recv().await {
            self.handle_request(request).await;
        }
    }

    async fn handle_request(&mut self, request: ManagerRequest) {
        match request {
            ManagerRequest::AddWatch { repo, respond_to } => {
                let _ = respond_to.send(self.add_watch(repo).await);
            }
            ManagerRequest::RemoveWatch { repo, respond_to } => {
                let _ = respond_to.send(self.remove_watch(repo).await);
            }
            ManagerRequest::ListWatches { respond_to } => {
                let _ = respond_to.send(self.list_watches());
            }
        }
    }

    async fn add_watch(&mut self, repo: PathBuf) -> ControlResponse {
        let repo = match RepoState::discover(&repo) {
            Ok(repo) => repo,
            Err(err) => return ControlResponse::error(err.to_string()),
        };
        let root = repo.root.clone();
        let was_active = self.watchers.contains_key(&root);
        let was_suppressed = self.suppressed_roots.contains(&root);

        if !was_active {
            if let Err(err) = self.ensure_active(repo) {
                return ControlResponse::error(err.to_string());
            }
        }

        let mut next_persisted = self.persisted_roots.clone();
        let added_to_config = next_persisted.insert(root.clone());
        if let Err(err) = self.persist_repo_set(&next_persisted) {
            if !was_active {
                self.watchers.remove(&root);
                self.sync_shared_repos().await;
            }
            return ControlResponse::error(err.to_string());
        }

        self.persisted_roots = next_persisted;
        self.suppressed_roots.remove(&root);
        self.sync_shared_repos().await;

        if !was_active {
            ControlResponse::success_message(format!("watch added for {}", root.display()))
        } else if was_suppressed {
            ControlResponse::success_message(format!("watch re-enabled for {}", root.display()))
        } else if added_to_config {
            ControlResponse::success_message(format!(
                "watch already active; persisted {}",
                root.display()
            ))
        } else {
            ControlResponse::success_message(format!("watch already active for {}", root.display()))
        }
    }

    async fn remove_watch(&mut self, repo: PathBuf) -> ControlResponse {
        let root = match normalize_repo_root(&repo) {
            Ok(root) => root,
            Err(err) => return ControlResponse::error(err.to_string()),
        };

        let was_active = self.watchers.contains_key(&root);
        let was_persisted = self.persisted_roots.contains(&root);
        let was_cli = self.cli_roots.contains(&root);

        if !was_active && !was_persisted && !was_cli {
            return ControlResponse::error(format!("watch not found for {}", root.display()));
        }

        if was_persisted {
            let mut next_persisted = self.persisted_roots.clone();
            next_persisted.remove(&root);
            if let Err(err) = self.persist_repo_set(&next_persisted) {
                return ControlResponse::error(err.to_string());
            }
            self.persisted_roots = next_persisted;
        }

        self.suppressed_roots.insert(root.clone());
        self.watchers.remove(&root);
        self.sync_shared_repos().await;

        if was_cli {
            ControlResponse::success_message(format!(
                "watch removed for {}; it will return if the daemon restarts with the same CLI repo list",
                root.display()
            ))
        } else {
            ControlResponse::success_message(format!("watch removed for {}", root.display()))
        }
    }

    fn list_watches(&self) -> ControlResponse {
        ControlResponse::list(self.watchers.keys().cloned().collect())
    }

    fn ensure_active(&mut self, repo: RepoState) -> NotifyResult<()> {
        if self.watchers.contains_key(&repo.root) {
            return Ok(());
        }

        let watcher = start_watcher(repo.clone(), self.raw_tx.clone())?;
        self.watchers.insert(
            repo.root.clone(),
            WatchRegistration {
                state: repo,
                _watcher: watcher,
            },
        );
        Ok(())
    }

    fn persist_repo_set(&self, repos: &BTreeSet<PathBuf>) -> io::Result<()> {
        self.config_store.save(&GongdConfig {
            repos: repos.iter().cloned().collect(),
        })
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

fn start_watcher(repo: RepoState, tx: mpsc::Sender<RawEvent>) -> NotifyResult<RecommendedWatcher> {
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

    use tokio::sync::{mpsc, RwLock};

    use super::WatchManager;
    use crate::{
        config::ConfigStore,
        test_support::{init_git_repo, TestDir},
    };

    #[tokio::test]
    async fn add_watch_persists_repo_and_remove_watch_disables_cli_watch() {
        let tmp = TestDir::new("gongd-watch-manager");
        let cli_repo = tmp.path().join("cli");
        let added_repo = tmp.path().join("added");
        init_git_repo(&cli_repo);
        init_git_repo(&added_repo);
        let cli_root = std::fs::canonicalize(&cli_repo).unwrap();
        let added_root = std::fs::canonicalize(&added_repo).unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let repos = Arc::new(RwLock::new(Vec::new()));
        let store = ConfigStore::new(tmp.path().join("gongd.json"));

        let mut manager = WatchManager::new(
            repos.clone(),
            raw_tx,
            vec![cli_repo.clone()],
            Vec::new(),
            store.clone(),
        );
        manager.initialize().await.unwrap();
        assert_eq!(store.load().unwrap().repos, vec![cli_root.clone()]);

        let add_response = manager.add_watch(added_repo.clone()).await;
        assert!(add_response.ok);
        assert_eq!(
            store.load().unwrap().repos,
            vec![added_root.clone(), cli_root.clone()]
        );

        let remove_response = manager.remove_watch(cli_repo.clone()).await;
        assert!(remove_response.ok);
        assert!(repos.read().await.iter().all(|repo| repo.root != cli_root));
        assert_eq!(store.load().unwrap().repos, vec![added_root]);
    }
}
