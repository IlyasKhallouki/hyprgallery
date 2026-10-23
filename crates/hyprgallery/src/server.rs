//! The control socket. A thread accepts connections and hands each request to
//! the GTK main loop, which owns all state; the connection's own thread waits
//! for the answer and writes it back.

use hyprgallery_core::ipc::{self, Reply, Request};
use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

pub struct Incoming {
    pub request: Request,
    /// Where the answer goes. For `Subscribe`, unused: the stream itself is
    /// handed over instead.
    pub reply: mpsc::Sender<Reply>,
    pub stream: Option<UnixStream>,
}

/// Holds the single-instance lock for as long as it lives.
pub struct Lock {
    _file: std::fs::File,
}

/// Take the daemon lock, or `None` if another daemon has it.
///
/// A lock file rather than "is the socket answering": two daemons started at
/// the same moment (Waybar and a key binding, at login) could both see no
/// answer, and the second would delete the first one's socket.
pub fn lock(socket: &Path) -> std::io::Result<Option<Lock>> {
    // `hyprgallery.sock.lock`, not `hyprgallery.lock`: Wayland compositors
    // keep `<display>.lock` files in the same directory and hold a lock on
    // them, so a display that happened to share our socket's stem would look
    // exactly like a running daemon.
    let path = std::path::PathBuf::from(format!("{}.lock", socket.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(taken.then_some(Lock { _file: file }))
}

pub fn bind(socket: &Path) -> std::io::Result<UnixListener> {
    // Holding the lock means any socket file here is a leftover.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub fn spawn(listener: UnixListener, to_main: async_channel::Sender<Incoming>) {
    std::thread::Builder::new()
        .name("hg-ipc".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let to_main = to_main.clone();
                let _ = std::thread::Builder::new()
                    .name("hg-conn".into())
                    .spawn(move || serve(stream, to_main));
            }
        })
        .expect("spawn ipc thread");
}

fn serve(stream: UnixStream, to_main: async_channel::Sender<Incoming>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let request: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            let _ = ipc::write_line(&stream, &Reply::error(format!("bad request: {e}")));
            return;
        }
    };
    let (reply, answer) = mpsc::channel();
    if request == Request::Subscribe {
        let _ = stream.set_read_timeout(None);
        let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
        let _ = to_main.send_blocking(Incoming { request, reply, stream: Some(stream) });
        return;
    }
    if to_main.send_blocking(Incoming { request, reply, stream: None }).is_err() {
        return;
    }
    // Long enough for HyDE to finish recolouring after a `set`.
    match answer.recv_timeout(Duration::from_secs(90)) {
        Ok(r) => {
            let _ = ipc::write_line(&stream, &r);
        }
        Err(_) => {
            let _ = ipc::write_line(&stream, &Reply::error("the daemon did not answer"));
        }
    }
}
