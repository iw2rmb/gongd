use std::{
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    path::Path,
};

use serde::{de::DeserializeOwned, Serialize};

use crate::{Client, ControlResponse, Error};

impl Client {
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

    fn connect_control(&self) -> Result<UnixStream, Error> {
        Ok(UnixStream::connect(&self.control_socket)?)
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

fn write_json_line(writer: &mut impl std::io::Write, value: &impl Serialize) -> Result<(), Error> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        test_support::{socket_path, spawn_control_server, SocketGuard},
        Client, ControlResponse, Error,
    };

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
}
