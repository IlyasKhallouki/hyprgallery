//! Talking to the daemon, and starting it when nobody has.

use anyhow::{Context, Result};
use hyprgallery_core::ipc::{self, IpcError, Reply, Request};
use hyprgallery_core::paths::{self, Paths};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Send `request`. With `spawn`, start the daemon first if it is not running,
/// so a fresh login needs no `exec-once` before the first click works.
pub fn send(request: Request, spawn: bool, timeout: Duration) -> Result<Reply> {
    let socket = paths::socket_path();
    match ipc::request(&socket, &request, timeout) {
        Err(IpcError::NotRunning) if spawn => {
            start_daemon()?;
            wait_for_daemon(Duration::from_secs(8))?;
            Ok(ipc::request(&socket, &request, timeout)?)
        }
        other => Ok(other?),
    }
}

pub fn not_running(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<IpcError>(), Some(IpcError::NotRunning))
}

/// Start `hyprgallery daemon` in its own session, logging to the state dir.
pub fn start_daemon() -> Result<()> {
    let exe = std::env::current_exe().context("finding our own executable")?;
    let log = Paths::from_env().log_file;
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let err = out.try_clone()?;
    let mut command = Command::new(exe);
    command.arg("daemon").stdin(Stdio::null()).stdout(out).stderr(err);
    // Its own session, so closing the terminal or restarting Waybar does not
    // take the gallery down with it.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().context("starting the daemon")?;
    Ok(())
}

pub fn wait_for_daemon(timeout: Duration) -> Result<()> {
    let socket = paths::socket_path();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if ipc::connect(&socket).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    let log = Paths::from_env().log_file;
    anyhow::bail!("the daemon did not start; see {}", log.display())
}
