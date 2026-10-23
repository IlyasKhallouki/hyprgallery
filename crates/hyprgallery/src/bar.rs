//! The Waybar module: one JSON line per change, for as long as Waybar runs it.

use crate::client;
use anyhow::Result;
use hyprgallery_core::config::{BarConfig, Config};
use hyprgallery_core::ipc::{self, IpcError, Status};
use hyprgallery_core::paths::{self, Paths};
use hyprgallery_core::rotation::describe_interval;
use std::io::Write;
use std::time::Duration;

pub fn run() -> Result<()> {
    let config = Config::load(&Paths::from_env().config_file).unwrap_or_default().bar;
    let socket = paths::socket_path();
    let mut stdout = std::io::stdout().lock();
    let mut last = String::new();
    loop {
        match ipc::subscribe(&socket) {
            Ok(statuses) => {
                for status in statuses {
                    let Ok(status) = status else { break };
                    let line = render(&status, &config);
                    // Waybar re-renders on every line, so only send changes.
                    if line != last {
                        writeln!(stdout, "{line}")?;
                        stdout.flush()?;
                        last = line;
                    }
                }
            }
            Err(IpcError::NotRunning) => {
                if let Err(e) = client::start_daemon()
                    .and_then(|_| client::wait_for_daemon(Duration::from_secs(8)))
                {
                    eprintln!("hyprgallery bar: {e:#}");
                }
                continue;
            }
            Err(e) => eprintln!("hyprgallery bar: {e}"),
        }
        // The daemon went away. Say so, then try again shortly; whatever
        // stopped it (an upgrade, a crash) is usually over by then.
        let line = offline(&config);
        if line != last {
            writeln!(stdout, "{line}")?;
            stdout.flush()?;
            last = line;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn offline(config: &BarConfig) -> String {
    serde_json::json!({
        "text": config.icon,
        "alt": "offline",
        "class": ["offline"],
        "tooltip": "hyprgallery is not running",
    })
    .to_string()
}

/// One Waybar line.
pub fn render(status: &Status, config: &BarConfig) -> String {
    let state = if status.applying {
        "applying"
    } else if status.error.is_some() {
        "error"
    } else if status.rotation.is_some() {
        "rotating"
    } else {
        "idle"
    };
    let mut class = vec![state];
    if status.visible {
        class.push("open");
    }
    let text = if status.rotation.is_some() { &config.rotating_icon } else { &config.icon };
    serde_json::json!({
        "text": text,
        "alt": state,
        "class": class,
        "tooltip": tooltip(status),
    })
    .to_string()
}

fn tooltip(status: &Status) -> String {
    let mut lines = Vec::new();
    match (&status.name, &status.album) {
        (Some(name), Some(album)) => lines.push(format!(
            "<b>{}</b>  <span alpha='65%'>{}</span>",
            escape(name),
            escape(album)
        )),
        (Some(name), None) => lines.push(format!("<b>{}</b>", escape(name))),
        _ => lines.push("No wallpaper chosen yet".into()),
    }
    if status.applying {
        lines.push("Applying\u{2026}".into());
    }
    if let Some(r) = &status.rotation {
        let order = if r.shuffle { "shuffling" } else { "rotating" };
        lines.push(format!(
            "{} {} {}, next at {}",
            capitalise(order),
            escape(&r.album),
            describe_interval(r.every_secs),
            clock(r.next_at)
        ));
    }
    if let Some((done, total)) = status.indexing {
        lines.push(format!("Preparing thumbnails, {done} of {total}"));
    }
    if let Some(e) = &status.error {
        lines.push(format!("<span color='#ff8a8a'>{}</span>", escape(e)));
    }
    lines.push("<span alpha='60%'>Click to browse</span>".into());
    lines.join("\n")
}

/// Plain text for `status` and `rotate`.
pub fn describe(status: &Status) -> String {
    let mut out = Vec::new();
    match (&status.name, &status.current) {
        (Some(name), Some(path)) => out.push(format!("{name}  ({})", path.display())),
        _ => out.push("no wallpaper set yet".into()),
    }
    if let Some(album) = &status.album {
        out.push(format!("album: {album}"));
    }
    if let Some(backend) = &status.backend {
        out.push(format!("backend: {backend}"));
    }
    match &status.rotation {
        Some(r) => out.push(format!(
            "rotation: {} {}{}, next at {}",
            r.album,
            describe_interval(r.every_secs),
            if r.shuffle { ", shuffled" } else { "" },
            clock(r.next_at)
        )),
        None => out.push("rotation: off".into()),
    }
    if let Some((done, total)) = status.indexing {
        out.push(format!("thumbnails: {done} of {total}"));
    }
    if let Some(e) = &status.error {
        out.push(format!("error: {e}"));
    }
    out.join("\n")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&#39;")
        .replace('"', "&quot;")
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Local wall-clock time, HH:MM.
fn clock(epoch: i64) -> String {
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "?".into();
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprgallery_core::ipc::RotationStatus;

    #[test]
    fn idle_line() {
        let status = Status {
            name: Some("Earth & sky".into()),
            album: Some("Space".into()),
            ..Status::default()
        };
        let line: serde_json::Value =
            serde_json::from_str(&render(&status, &BarConfig::default())).unwrap();
        assert_eq!(line["alt"], "idle");
        assert_eq!(line["class"], serde_json::json!(["idle"]));
        assert!(line["tooltip"].as_str().unwrap().starts_with("<b>Earth &amp; sky</b>"));
    }

    #[test]
    fn rotating_uses_its_own_icon_and_class() {
        let config = BarConfig { icon: "W".into(), rotating_icon: "R".into() };
        let status = Status {
            rotation: Some(RotationStatus {
                album: "Space".into(),
                every_secs: 3600,
                shuffle: true,
                next_at: 0,
            }),
            visible: true,
            ..Status::default()
        };
        let line: serde_json::Value = serde_json::from_str(&render(&status, &config)).unwrap();
        assert_eq!(line["text"], "R");
        assert_eq!(line["class"], serde_json::json!(["rotating", "open"]));
        assert!(line["tooltip"].as_str().unwrap().contains("Shuffling Space hourly"));
    }

    #[test]
    fn errors_win_over_rotation() {
        let status = Status {
            error: Some("awww: no such output".into()),
            rotation: Some(RotationStatus {
                album: "x".into(),
                every_secs: 900,
                shuffle: false,
                next_at: 0,
            }),
            ..Status::default()
        };
        let line: serde_json::Value =
            serde_json::from_str(&render(&status, &BarConfig::default())).unwrap();
        assert_eq!(line["alt"], "error");
    }
}
