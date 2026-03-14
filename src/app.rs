use std::{io, sync::Arc, time::Duration};

use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

use crate::{
    args::Args,
    config::ConfigStore,
    event::{translate_event, Deduper, SharedDeduper},
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

    let mut manager = WatchManager::new(
        repos.clone(),
        raw_tx,
        args.repos,
        config_store,
    );
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
