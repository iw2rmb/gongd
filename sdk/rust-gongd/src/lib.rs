use std::{
    fmt,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub const DEFAULT_EVENT_SOCKET: &str = "/tmp/gongd.sock";
pub const DEFAULT_CONTROL_SOCKET: &str = "/tmp/gongd.ctl.sock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Event {
    pub repo: String,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub path: Option<String>,
    pub git_path: Option<String>,
    pub ts_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    FileCreated,
    FileModified,
    FileDeleted,
    FileRenamed,
    DirCreated,
    DirDeleted,
    DirRenamed,
    RepoHeadChanged,
    RepoIndexChanged,
    RepoRefsChanged,
    RepoPackedRefsChanged,
    RepoChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    Daemon(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Json(err) => write!(f, "{err}"),
            Self::Daemon(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    pub event_socket: PathBuf,
    pub control_socket: PathBuf,
}

impl Client {
    pub fn new() -> Self {
        Self {
            event_socket: PathBuf::from(DEFAULT_EVENT_SOCKET),
            control_socket: PathBuf::from(DEFAULT_CONTROL_SOCKET),
        }
    }

    pub fn with_sockets(
        event_socket: impl Into<PathBuf>,
        control_socket: impl Into<PathBuf>,
    ) -> Self {
        Self {
            event_socket: event_socket.into(),
            control_socket: control_socket.into(),
        }
    }

    pub fn subscribe(&self) -> Result<EventStream, Error> {
        let stream = self.connect_event()?;
        Ok(EventStream {
            reader: BufReader::new(stream),
            line_buf: String::new(),
        })
    }

    pub fn add_watch(&self, repo: impl AsRef<Path>) -> Result<ControlResponse, Error> {
        self.send_repo_control(RepoControlOp::AddWatch, repo)
    }

    pub fn remove_watch(&self, repo: impl AsRef<Path>) -> Result<ControlResponse, Error> {
        self.send_repo_control(RepoControlOp::RemoveWatch, repo)
    }

    pub fn list_watches(&self) -> Result<Vec<String>, Error> {
        let response: ControlResponse = self.send_control(ControlRequest {
            op: "list_watches",
            repo: None,
        })?;
        Ok(response.repos.unwrap_or_default())
    }

    fn send_control<T: Serialize>(&self, request: T) -> Result<ControlResponse, Error> {
        let mut stream = self.connect_control()?;
        write_json_line(&mut stream, &request)?;

        let mut reader = BufReader::new(stream);
        let response: ControlResponse = read_json_line(&mut reader)?;
        if response.ok {
            Ok(response)
        } else {
            Err(Error::Daemon(response.error.clone().unwrap_or_else(|| {
                "gongd returned an unsuccessful response".to_owned()
            })))
        }
    }

    fn send_repo_control(
        &self,
        op: RepoControlOp,
        repo: impl AsRef<Path>,
    ) -> Result<ControlResponse, Error> {
        self.send_control(ControlRequest {
            op: op.as_str(),
            repo: Some(repo.as_ref().display().to_string()),
        })
    }

    fn connect_event(&self) -> Result<UnixStream, Error> {
        Self::connect(&self.event_socket)
    }

    fn connect_control(&self) -> Result<UnixStream, Error> {
        Self::connect(&self.control_socket)
    }

    fn connect(path: &Path) -> Result<UnixStream, Error> {
        Ok(UnixStream::connect(path)?)
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EventStream {
    reader: BufReader<UnixStream>,
    line_buf: String,
}

impl EventStream {
    pub fn next_event(&mut self) -> Result<Option<Event>, Error> {
        read_optional_json_line(&mut self.reader, &mut self.line_buf)
    }
}

#[derive(Serialize)]
struct ControlRequest<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
}

#[derive(Clone, Copy)]
enum RepoControlOp {
    AddWatch,
    RemoveWatch,
}

impl RepoControlOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::AddWatch => "add_watch",
            Self::RemoveWatch => "remove_watch",
        }
    }
}

fn read_json_line<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<T, Error> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(line.trim())?)
}

fn read_optional_json_line<T: DeserializeOwned>(
    reader: &mut impl BufRead,
    line_buf: &mut String,
) -> Result<Option<T>, Error> {
    line_buf.clear();
    let read = reader.read_line(line_buf)?;
    if read == 0 {
        return Ok(None);
    }

    let line = line_buf.trim();
    if line.is_empty() {
        return Ok(None);
    }

    Ok(Some(serde_json::from_str(line)?))
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> Result<(), Error> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{write_json_line, Client, ControlResponse, Error, EventType};

    #[test]
    fn add_watch_sends_expected_request() {
        let control_socket = socket_path("ctl-add");
        let _guard = SocketGuard::new(&control_socket);
        let handle = spawn_control_server(
            &control_socket,
            |value| {
                assert_eq!(value["op"], "add_watch");
                assert_eq!(value["repo"], "/tmp/repo");
            },
            ControlResponse {
                ok: true,
                message: Some("watch added".to_owned()),
                error: None,
                repos: None,
            },
        );

        let client = Client::with_sockets("/tmp/unused.sock", &control_socket);
        let response = client.add_watch("/tmp/repo").unwrap();
        assert_eq!(response.message.as_deref(), Some("watch added"));
        handle.join().unwrap();
    }

    #[test]
    fn list_watches_returns_repo_list() {
        let control_socket = socket_path("ctl-list");
        let _guard = SocketGuard::new(&control_socket);
        let handle = spawn_control_server(
            &control_socket,
            |value| {
                assert_eq!(value["op"], "list_watches");
            },
            ControlResponse {
                ok: true,
                message: None,
                error: None,
                repos: Some(vec!["/tmp/a".to_owned(), "/tmp/b".to_owned()]),
            },
        );

        let client = Client::with_sockets("/tmp/unused.sock", &control_socket);
        let repos = client.list_watches().unwrap();
        assert_eq!(repos, vec!["/tmp/a", "/tmp/b"]);
        handle.join().unwrap();
    }

    #[test]
    fn remove_watch_surfaces_daemon_error() {
        let control_socket = socket_path("ctl-remove");
        let _guard = SocketGuard::new(&control_socket);
        let handle = spawn_control_server(
            &control_socket,
            |_| {},
            ControlResponse {
                ok: false,
                message: None,
                error: Some("watch not found".to_owned()),
                repos: None,
            },
        );

        let client = Client::with_sockets("/tmp/unused.sock", &control_socket);
        let err = client.remove_watch("/tmp/repo").unwrap_err();
        match err {
            Error::Daemon(message) => assert_eq!(message, "watch not found"),
            other => panic!("unexpected error: {other}"),
        }
        handle.join().unwrap();
    }

    #[test]
    fn subscribe_reads_events_until_eof() {
        let event_socket = socket_path("evt");
        let _guard = SocketGuard::new(&event_socket);
        let listener = UnixListener::bind(&event_socket).unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"{\"repo\":\"/tmp/repo\",\"type\":\"file_modified\",\"path\":\"main.go\",\"git_path\":null,\"ts_unix_ms\":1}\n")
                .unwrap();
            stream
                .write_all(b"{\"repo\":\"/tmp/repo\",\"type\":\"repo_head_changed\",\"path\":null,\"git_path\":\"HEAD\",\"ts_unix_ms\":2}\n")
                .unwrap();
        });

        let client = Client::with_sockets(&event_socket, "/tmp/unused.sock");
        let mut stream = client.subscribe().unwrap();

        let first = stream.next_event().unwrap().unwrap();
        assert_eq!(first.event_type, EventType::FileModified);
        assert_eq!(first.path.as_deref(), Some("main.go"));

        let second = stream.next_event().unwrap().unwrap();
        assert_eq!(second.event_type, EventType::RepoHeadChanged);
        assert_eq!(second.git_path.as_deref(), Some("HEAD"));

        assert!(stream.next_event().unwrap().is_none());
        handle.join().unwrap();
    }

    fn spawn_control_server(
        socket: &str,
        assert_request: impl FnOnce(serde_json::Value) + Send + 'static,
        response: ControlResponse,
    ) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(socket).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_request(value);

            let mut stream = reader.into_inner();
            write_json_line(&mut stream, &response).unwrap();
        })
    }

    fn socket_path(name: &str) -> String {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("/tmp/gongd-sdk-{name}-{unique}.sock")
    }

    struct SocketGuard {
        path: String,
    }

    impl SocketGuard {
        fn new(path: &str) -> Self {
            let _ = fs::remove_file(path);
            Self {
                path: path.to_owned(),
            }
        }
    }

    impl Drop for SocketGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }
}
