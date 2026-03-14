use std::{io, sync::Arc, time::Duration};

use notify::{Event, EventKind};
use tokio::sync::oneshot;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

use crate::{
    args::Args,
    config::ConfigStore,
    event::{translate_event, Deduper, SharedDeduper},
    repo::RepoState,
    server::{control_socket_server, event_socket_server, prepare_socket_path},
    watch::{ManagerRequest, SharedRepos, WatchManager},
};

pub async fn run(args: Args) -> io::Result<()> {
    if args.socket == args.control_socket {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "event socket and control socket must be different paths",
        ));
    }

    let config_store = ConfigStore::new(args.config_path()?);

    prepare_socket_path(&args.socket)?;
    prepare_socket_path(&args.control_socket)?;

    let (raw_tx, mut raw_rx) = mpsc::channel(1024);
    let (broadcast_tx, _) = broadcast::channel::<String>(4096);
    let repos: SharedRepos = Arc::new(RwLock::new(Vec::new()));
    let deduper: SharedDeduper = Arc::new(Mutex::new(Deduper::new(Duration::from_millis(
        args.debounce_ms,
    ))));
    let (manager_tx, manager_rx) = mpsc::channel::<ManagerRequest>(128);

    let mut manager = WatchManager::new(repos.clone(), raw_tx, args.repos, config_store);
    manager.initialize().await?;

    let event_socket = args.socket.clone();
    let event_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        if let Err(err) = event_socket_server(event_socket, event_tx).await {
            eprintln!("event socket server failed: {err}");
        }
    });

    let control_socket = args.control_socket.clone();
    let control_tx = manager_tx.clone();
    tokio::spawn(async move {
        if let Err(err) = control_socket_server(control_socket, control_tx).await {
            eprintln!("control socket server failed: {err}");
        }
    });

    tokio::spawn(manager.run(manager_rx));

    while let Some(msg) = raw_rx.recv().await {
        let snapshot = repos.read().await.clone();
        remove_missing_watches(&manager_tx, &snapshot, &msg).await;

        match msg {
            Ok(event) => {
                let snapshot = repos.read().await.clone();
                for wire in translate_event(&snapshot, event, deduper.clone()).await {
                    match serde_json::to_string(&wire) {
                        Ok(mut line) => {
                            line.push('\n');
                            let _ = broadcast_tx.send(line);
                        }
                        Err(err) => eprintln!("serialization failed: {err}"),
                    }
                }
            }
            Err(err) => eprintln!("watch error: {err}"),
        }
    }

    Ok(())
}

async fn remove_missing_watches(
    manager_tx: &mpsc::Sender<ManagerRequest>,
    repos: &[RepoState],
    event: &notify::Result<Event>,
) {
    if !should_prune_missing_watches(event) {
        return;
    }

    let missing_roots: Vec<_> = repos
        .iter()
        .filter(|repo| !repo.root.exists() || !repo.git_dir.exists())
        .map(|repo| repo.root.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    for repo in missing_roots {
        let (respond_to, response_rx) = oneshot::channel();
        if manager_tx
            .send(ManagerRequest::RemoveWatch { repo, respond_to })
            .await
            .is_err()
        {
            return;
        }

        let _ = response_rx.await;
    }
}

fn should_prune_missing_watches(event: &notify::Result<Event>) -> bool {
    match event {
        Ok(event) => matches!(event.kind, EventKind::Remove(_)),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use notify::{
        event::{EventAttributes, RemoveKind},
        Event, EventKind,
    };
    use tokio::sync::{mpsc, RwLock};

    use super::remove_missing_watches;
    use crate::{
        config::ConfigStore,
        test_support::{init_git_repo, wait_for, TestDir},
        watch::WatchManager,
    };

    #[tokio::test]
    async fn remove_missing_watches_persists_deleted_repo() {
        let tmp = TestDir::new("gongd-app-missing-watch");
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let repo_root = std::fs::canonicalize(&repo).unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(16);
        let repos = Arc::new(RwLock::new(Vec::new()));
        let store = ConfigStore::new(tmp.path().join(".gong").join("config.json"));

        let mut manager =
            WatchManager::new(repos.clone(), raw_tx, vec![repo.clone()], store.clone());
        manager.initialize().await.unwrap();

        let (manager_tx, manager_rx) = mpsc::channel(16);
        let manager_handle = tokio::spawn(manager.run(manager_rx));

        wait_for(|| store.load().ok().map(|config| config.repos) == Some(vec![repo_root.clone()]))
            .await;
        wait_for(|| {
            repos
                .try_read()
                .map(|repos| repos.iter().any(|repo| repo.root == repo_root))
                .unwrap_or(false)
        })
        .await;

        fs::remove_dir_all(&repo_root).unwrap();

        let event = Ok(Event {
            kind: EventKind::Remove(RemoveKind::Folder),
            paths: vec![repo_root.clone()],
            attrs: EventAttributes::default(),
        });
        let snapshot = repos.read().await.clone();
        remove_missing_watches(&manager_tx, &snapshot, &event).await;

        wait_for(|| store.load().ok().map(|config| config.repos) == Some(Vec::new())).await;
        wait_for(|| {
            repos
                .try_read()
                .map(|repos| repos.is_empty())
                .unwrap_or(false)
        })
        .await;

        manager_handle.abort();
        let _ = manager_handle.await;
    }
}
