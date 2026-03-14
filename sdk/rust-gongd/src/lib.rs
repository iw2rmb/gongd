mod control;
mod watch;

use std::{fmt, io, path::PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_EVENT_SOCKET: &str = "/tmp/gongd.sock";
pub const DEFAULT_CONTROL_SOCKET: &str = "/tmp/gongd.ctl.sock";
pub const VERSION: &str = "v0.1.0";

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
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::VERSION;

    #[test]
    fn version_matches_release_tag() {
        assert_eq!(VERSION, "v0.1.0");
    }
}

#[cfg(test)]
mod test_support {
    use std::{
        fs,
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde::Serialize;

    use crate::ControlResponse;

    pub fn write_json_line(writer: &mut impl std::io::Write, value: &impl Serialize) {
        serde_json::to_writer(&mut *writer, value).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
    }

    pub fn spawn_control_server(
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
            write_json_line(&mut stream, &response);
        })
    }

    pub fn socket_path(name: &str) -> String {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("/tmp/gongd-sdk-{name}-{unique}.sock")
    }

    pub struct SocketGuard {
        path: String,
    }

    impl SocketGuard {
        pub fn new(path: &str) -> Self {
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
