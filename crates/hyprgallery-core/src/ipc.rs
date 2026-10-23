//! The control socket's protocol: one JSON object per line.
//!
//! Every client command is one request and one reply on a fresh connection,
//! except `subscribe`, which keeps the connection and receives a [`Status`]
//! line whenever something the bar shows changes.

use crate::config::Layout;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Toggle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<Layout>,
    },
    Show {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<Layout>,
    },
    Hide,
    /// Apply a file. Replies once it is on screen, or with why it is not.
    Set {
        path: PathBuf,
    },
    Next {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        album: Option<String>,
    },
    Prev {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        album: Option<String>,
    },
    Random {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        album: Option<String>,
    },
    Rotate(RotateRequest),
    Restore,
    Reindex,
    Status,
    Subscribe,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RotateRequest {
    On {
        every_secs: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        album: Option<String>,
        #[serde(default)]
        shuffle: bool,
    },
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

impl Reply {
    pub fn ok() -> Self {
        Self { ok: true, error: None, status: None }
    }

    pub fn status(status: Status) -> Self {
        Self { ok: true, error: None, status: Some(status) }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self { ok: false, error: Some(message.into()), status: None }
    }
}

/// Everything a status bar might show.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub current: Option<PathBuf>,
    pub name: Option<String>,
    pub album: Option<String>,
    /// A change is in progress. HyDE takes a few seconds to recolour.
    pub applying: bool,
    pub rotation: Option<RotationStatus>,
    /// Thumbnails done and total, while the first scan is still going.
    pub indexing: Option<(usize, usize)>,
    pub visible: bool,
    pub backend: Option<String>,
    /// The last thing that went wrong, until something goes right.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RotationStatus {
    pub album: String,
    pub every_secs: u64,
    pub shuffle: bool,
    pub next_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("hyprgallery is not running")]
    NotRunning,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("bad message from the daemon: {0}")]
    Protocol(String),
    #[error("{0}")]
    Refused(String),
}

pub fn connect(socket: &Path) -> Result<UnixStream, IpcError> {
    UnixStream::connect(socket).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            IpcError::NotRunning
        }
        _ => IpcError::Io(e),
    })
}

pub fn write_line<T: Serialize>(mut w: impl Write, value: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// Send one request and wait for its reply.
pub fn request(socket: &Path, req: &Request, timeout: Duration) -> Result<Reply, IpcError> {
    let stream = connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    write_line(&stream, req)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    if line.is_empty() {
        return Err(IpcError::Protocol("connection closed without a reply".into()));
    }
    let reply: Reply =
        serde_json::from_str(&line).map_err(|e| IpcError::Protocol(e.to_string()))?;
    match reply.ok {
        true => Ok(reply),
        false => Err(IpcError::Refused(reply.error.unwrap_or_else(|| "refused".into()))),
    }
}

/// Subscribe and yield each status as it arrives, until the daemon goes away.
pub fn subscribe(
    socket: &Path,
) -> Result<impl Iterator<Item = Result<Status, IpcError>>, IpcError> {
    let stream = connect(socket)?;
    write_line(&stream, &Request::Subscribe)?;
    Ok(BufReader::new(stream).lines().map(|line| {
        let line = line?;
        serde_json::from_str(&line).map_err(|e| IpcError::Protocol(e.to_string()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_readable_json() {
        let json =
            serde_json::to_string(&Request::Toggle { layout: Some(Layout::Popover) }).unwrap();
        assert_eq!(json, r#"{"cmd":"toggle","layout":"popover"}"#);
        let json = serde_json::to_string(&Request::Toggle { layout: None }).unwrap();
        assert_eq!(json, r#"{"cmd":"toggle"}"#);
        let json = serde_json::to_string(&Request::Rotate(RotateRequest::Off)).unwrap();
        assert_eq!(json, r#"{"cmd":"rotate","action":"off"}"#);
    }

    #[test]
    fn every_request_round_trips() {
        let all = [
            Request::Toggle { layout: None },
            Request::Show { layout: Some(Layout::Overlay) },
            Request::Hide,
            Request::Set { path: "/w/a.jpg".into() },
            Request::Next { album: Some("Space".into()) },
            Request::Prev { album: None },
            Request::Random { album: None },
            Request::Rotate(RotateRequest::On { every_secs: 900, album: None, shuffle: true }),
            Request::Rotate(RotateRequest::Off),
            Request::Restore,
            Request::Reindex,
            Request::Status,
            Request::Subscribe,
            Request::Quit,
        ];
        for req in all {
            let json = serde_json::to_string(&req).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req, "{json}");
        }
    }

    #[test]
    fn talks_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let req: Request = serde_json::from_str(&line).unwrap();
            assert_eq!(req, Request::Status);
            write_line(&stream, &Reply::status(Status { visible: true, ..Status::default() }))
                .unwrap();
        });
        let reply = request(&socket, &Request::Status, Duration::from_secs(2)).unwrap();
        assert!(reply.status.unwrap().visible);
        server.join().unwrap();
    }

    #[test]
    fn no_socket_means_not_running() {
        let err =
            request(Path::new("/nonexistent/hg.sock"), &Request::Status, Duration::from_secs(1));
        assert!(matches!(err, Err(IpcError::NotRunning)));
    }
}
