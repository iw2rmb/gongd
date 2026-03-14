use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};

use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc, oneshot},
};

use crate::{
    protocol::{ControlRequest, ControlResponse},
    watch::ManagerRequest,
};

pub fn prepare_socket_path(socket_path: &Path) -> io::Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    Ok(())
}

pub async fn event_socket_server(
    socket_path: PathBuf,
    tx: broadcast::Sender<String>,
) -> io::Result<()> {
    serve_unix_clients(
        socket_path,
        tx,
        |stream, tx| async move { event_client_writer(stream, tx.subscribe()).await },
        "event client disconnected",
    )
    .await
}

pub async fn control_socket_server(
    socket_path: PathBuf,
    tx: mpsc::Sender<ManagerRequest>,
) -> io::Result<()> {
    serve_unix_clients(
        socket_path,
        tx,
        handle_control_client,
        "control client disconnected",
    )
    .await
}

async fn serve_unix_clients<T, F, Fut>(
    socket_path: PathBuf,
    state: T,
    handler: F,
    disconnect_message: &'static str,
) -> io::Result<()>
where
    T: Clone + Send + 'static,
    F: Fn(UnixStream, T) -> Fut + Copy + Send + 'static,
    Fut: Future<Output = io::Result<()>> + Send + 'static,
{
    let listener = UnixListener::bind(&socket_path)?;
    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handler(stream, state).await {
                eprintln!("{disconnect_message}: {err}");
            }
        });
    }
}

async fn event_client_writer(
    mut stream: UnixStream,
    mut rx: broadcast::Receiver<String>,
) -> io::Result<()> {
    loop {
        match rx.recv().await {
            Ok(line) => stream.write_all(line.as_bytes()).await?,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn handle_control_client(
    stream: UnixStream,
    tx: mpsc::Sender<ManagerRequest>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }

    let request: ControlRequest = match serde_json::from_str(line.trim()) {
        Ok(request) => request,
        Err(err) => {
            write_json_line(
                &mut writer,
                &ControlResponse::error(format!("invalid request: {err}")),
            )
            .await?;
            return Ok(());
        }
    };

    let (respond_to, response_rx) = oneshot::channel();
    let request = match request {
        ControlRequest::AddWatch { repo } => ManagerRequest::AddWatch { repo, respond_to },
        ControlRequest::RemoveWatch { repo } => ManagerRequest::RemoveWatch { repo, respond_to },
        ControlRequest::ListWatches => ManagerRequest::ListWatches { respond_to },
    };

    if tx.send(request).await.is_err() {
        write_json_line(
            &mut writer,
            &ControlResponse::error("watch manager is unavailable"),
        )
        .await?;
        return Ok(());
    }

    let response = response_rx
        .await
        .unwrap_or_else(|_| ControlResponse::error("watch manager did not respond"));
    write_json_line(&mut writer, &response).await
}

async fn write_json_line<W, T>(writer: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let mut line = serde_json::to_string(value).map_err(|err| io::Error::other(err.to_string()))?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
        sync::{broadcast, mpsc, RwLock},
        time::{sleep, Duration},
    };

    use super::{control_socket_server, event_socket_server, prepare_socket_path};
    use crate::{
        config::ConfigStore,
        protocol::ControlResponse,
        test_support::{init_git_repo, TestDir},
        watch::WatchManager,
    };

    #[tokio::test]
    async fn control_socket_adds_lists_and_removes_watches() {
        let tmp = TestDir::new("gongd-control-socket");
        let repo = tmp.path().join("repo");
        let control_socket = short_socket_path("ctl");
        init_git_repo(&repo);
        let repo_root = std::fs::canonicalize(&repo).unwrap();

        let (raw_tx, _raw_rx) = mpsc::channel(32);
        let repos = Arc::new(RwLock::new(Vec::new()));
        let store = ConfigStore::new(tmp.path().join(".gong").join("config.json"));
        let mut manager = WatchManager::new(repos.clone(), raw_tx, Vec::new(), store.clone());
        manager.initialize().await.unwrap();

        let (manager_tx, manager_rx) = mpsc::channel(32);
        let manager_handle = tokio::spawn(manager.run(manager_rx));
        prepare_socket_path(&control_socket).unwrap();
        let server_handle = tokio::spawn(control_socket_server(control_socket.clone(), manager_tx));
        wait_for_socket(&control_socket).await;

        let add = send_request(
            &control_socket,
            &format!(r#"{{"op":"add_watch","repo":"{}"}}"#, repo.display()),
        )
        .await;
        assert!(add.ok);

        wait_for_repos(&control_socket, vec![repo_root.display().to_string()]).await;

        let remove = send_request(
            &control_socket,
            &format!(r#"{{"op":"remove_watch","repo":"{}"}}"#, repo.display()),
        )
        .await;
        assert!(remove.ok);

        wait_for_repos(&control_socket, Vec::new()).await;
        assert_eq!(
            store.load().unwrap().repos,
            Vec::<std::path::PathBuf>::new()
        );

        server_handle.abort();
        manager_handle.abort();
    }

    #[tokio::test]
    async fn event_socket_broadcasts_same_stream_to_multiple_clients() {
        let socket = short_socket_path("evt");
        prepare_socket_path(&socket).unwrap();

        let (tx, _) = broadcast::channel::<String>(16);
        let server_handle = tokio::spawn(event_socket_server(socket.clone(), tx.clone()));
        wait_for_socket(&socket).await;

        let mut client_a = UnixStream::connect(&socket).await.unwrap();
        let mut client_b = UnixStream::connect(&socket).await.unwrap();
        tx.send("{\"ok\":true}\n".to_owned()).unwrap();

        let mut line_a = String::new();
        let mut line_b = String::new();
        BufReader::new(&mut client_a)
            .read_line(&mut line_a)
            .await
            .unwrap();
        BufReader::new(&mut client_b)
            .read_line(&mut line_b)
            .await
            .unwrap();

        assert_eq!(line_a, line_b);

        server_handle.abort();
    }

    fn short_socket_path(kind: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        PathBuf::from(format!("/tmp/gongd-{kind}-{unique}.sock"))
    }

    async fn wait_for_socket(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
        panic!("socket was not created: {}", path.display());
    }

    async fn send_request(socket: &Path, request: &str) -> ControlResponse {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    async fn wait_for_repos(socket: &Path, expected: Vec<String>) {
        for _ in 0..100 {
            let response = send_request(socket, r#"{"op":"list_watches"}"#).await;
            if response.repos == Some(expected.clone()) {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }

        panic!("watch list did not converge");
    }
}
