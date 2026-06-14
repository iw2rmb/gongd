use std::{io, sync::Arc, time::Duration};

use notify::EventKind;
use tokio::sync::oneshot;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

use crate::{
    args::Args,
    config::ConfigStore,
    event::{translate_event, Deduper, SharedDeduper},
    folder::MonitoredFolder,
    server::{control_socket_server, event_socket_server, prepare_socket_path},
    watch::{ManagerRequest, RawEvent, SharedFolders, WatchManager},
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
    let folders: SharedFolders = Arc::new(RwLock::new(Vec::new()));
    let deduper: SharedDeduper = Arc::new(Mutex::new(Deduper::new(Duration::from_millis(
        args.debounce_ms,
    ))));
    let (manager_tx, manager_rx) = mpsc::channel::<ManagerRequest>(128);

    let mut manager = WatchManager::new(folders.clone(), raw_tx, args.folders, config_store);
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
        let snapshot = folders.read().await.clone();
        remove_missing_watches(&manager_tx, &snapshot, &msg).await;

        match msg {
            Ok(event) => {
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
    folders: &[MonitoredFolder],
    event: &RawEvent,
) {
    if !should_prune_missing_watches(event) {
        return;
    }

    let missing_roots: Vec<_> = folders
        .iter()
        .filter(|folder| !folder.root.is_dir())
        .map(|folder| folder.root.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    for folder in missing_roots {
        let (respond_to, response_rx) = oneshot::channel();
        if manager_tx
            .send(ManagerRequest::RemoveWatch { folder, respond_to })
            .await
            .is_err()
        {
            return;
        }

        let _ = response_rx.await;
    }
}

fn should_prune_missing_watches(event: &RawEvent) -> bool {
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
        test_support::{init_git_folder, wait_for, TestDir},
        watch::WatchManager,
    };

    #[tokio::test]
    async fn remove_missing_watches_prunes_only_missing_roots() {
        for (case, remove_git_dir, expected_config_empty) in [
            ("missing-root", false, true),
            ("missing-git-dir", true, false),
        ] {
            let tmp = TestDir::new(&format!("gongd-app-{case}"));
            let folder = tmp.path().join("folder");
            init_git_folder(&folder);
            let folder_root = std::fs::canonicalize(&folder).unwrap();

            let (raw_tx, _raw_rx) = mpsc::channel(16);
            let folders = Arc::new(RwLock::new(Vec::new()));
            let store = ConfigStore::new(tmp.path().join(".gong").join("config.json"));

            let mut manager =
                WatchManager::new(folders.clone(), raw_tx, vec![folder.clone()], store.clone());
            manager.initialize().await.unwrap();

            let (manager_tx, manager_rx) = mpsc::channel(16);
            let manager_handle = tokio::spawn(manager.run(manager_rx));

            wait_for(|| {
                store.load().ok().map(|config| config.folders) == Some(vec![folder.clone()])
            })
            .await;
            wait_for(|| {
                folders
                    .try_read()
                    .map(|folders| folders.iter().any(|folder| folder.root == folder_root))
                    .unwrap_or(false)
            })
            .await;

            if remove_git_dir {
                fs::remove_dir_all(folder_root.join(".git")).unwrap();
            } else {
                fs::remove_dir_all(&folder_root).unwrap();
            }

            let event = Ok(Event {
                kind: EventKind::Remove(RemoveKind::Folder),
                paths: vec![folder_root.clone()],
                attrs: EventAttributes::default(),
            });
            let snapshot = folders.read().await.clone();
            remove_missing_watches(&manager_tx, &snapshot, &event).await;

            let expected_config = if expected_config_empty {
                Vec::new()
            } else {
                vec![folder.clone()]
            };
            wait_for(|| {
                store.load().ok().map(|config| config.folders) == Some(expected_config.clone())
            })
            .await;

            manager_handle.abort();
            let _ = manager_handle.await;
        }
    }
}
