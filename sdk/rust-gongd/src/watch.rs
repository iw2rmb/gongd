use std::{
    io::{self, BufRead, BufReader},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use serde::de::DeserializeOwned;

use crate::{Client, Error, Event};

const RECONNECT_DELAY: Duration = Duration::from_millis(100);

impl Client {
    pub fn subscribe(&self) -> Result<EventStream, Error> {
        let stream = self.connect_event()?;
        Ok(EventStream {
            socket_path: self.event_socket.clone(),
            reader: BufReader::new(stream),
            line_buf: String::new(),
        })
    }

    fn connect_event(&self) -> Result<UnixStream, Error> {
        Ok(UnixStream::connect(&self.event_socket)?)
    }
}

pub struct EventStream {
    socket_path: PathBuf,
    reader: BufReader<UnixStream>,
    line_buf: String,
}

impl EventStream {
    pub fn next_event(&mut self) -> Result<Option<Event>, Error> {
        loop {
            match read_optional_json_line(&mut self.reader, &mut self.line_buf) {
                Ok(Some(event)) => return Ok(Some(event)),
                Ok(None) => self.reader = reconnect_event_reader(&self.socket_path)?,
                Err(Error::Io(err)) if is_reconnectable_io_error(&err) => {
                    self.reader = reconnect_event_reader(&self.socket_path)?;
                }
                Err(err) => return Err(err),
            }
        }
    }
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

fn reconnect_event_reader(socket_path: &Path) -> Result<BufReader<UnixStream>, Error> {
    loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(BufReader::new(stream)),
            Err(err) if is_reconnectable_io_error(&err) => thread::sleep(RECONNECT_DELAY),
            Err(err) => return Err(err.into()),
        }
    }
}

fn is_reconnectable_io_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::AddrNotAvailable
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, os::unix::net::UnixListener, sync::mpsc, thread, time::Duration};

    use crate::{
        test_support::{socket_path, SocketGuard},
        Client, EventType,
    };

    #[test]
    fn subscribe_reconnects_after_stream_eof() {
        let event_socket = socket_path("evt");
        let _guard = SocketGuard::new(&event_socket);
        let (ready_tx, ready_rx) = mpsc::channel();
        let listener = UnixListener::bind(&event_socket).unwrap();
        let server_socket = event_socket.clone();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"{\"repo\":\"/tmp/repo\",\"type\":\"file_modified\",\"path\":\"main.go\",\"git_path\":null,\"ts_unix_ms\":1}\n")
                .unwrap();
            drop(stream);
            drop(listener);

            let _ = fs::remove_file(&server_socket);
            let listener = UnixListener::bind(&server_socket).unwrap();
            ready_tx.send(()).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"{\"repo\":\"/tmp/repo\",\"type\":\"repo_head_changed\",\"path\":null,\"git_path\":\"HEAD\",\"ts_unix_ms\":2}\n")
                .unwrap();
        });

        let client = Client::with_sockets(&event_socket, "/tmp/unused.sock");
        let mut stream = client.subscribe().unwrap();

        let first = stream.next_event().unwrap().unwrap();
        assert_eq!(first.event_type, EventType::FileModified);
        assert_eq!(first.path.as_deref(), Some("main.go"));

        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = stream.next_event().unwrap().unwrap();
        assert_eq!(second.event_type, EventType::RepoHeadChanged);
        assert_eq!(second.git_path.as_deref(), Some("HEAD"));
        handle.join().unwrap();
    }
}
