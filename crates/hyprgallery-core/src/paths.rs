//! Where things live on disk, following the XDG base directory spec.

use std::env;
use std::path::{Path, PathBuf};

/// Every location the daemon reads or writes, resolved once.
///
/// Passed around rather than looked up on demand so tests can point the whole
/// thing at a temporary directory without touching the process environment.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub style_file: PathBuf,
    pub state_file: PathBuf,
    pub cache_dir: PathBuf,
    pub socket: PathBuf,
    pub log_file: PathBuf,
}

impl Paths {
    pub fn from_env() -> Self {
        let config = xdg("XDG_CONFIG_HOME", ".config").join("hyprgallery");
        let state = xdg("XDG_STATE_HOME", ".local/state").join("hyprgallery");
        Self {
            config_file: config.join("config.toml"),
            style_file: config.join("style.css"),
            state_file: state.join("state.json"),
            cache_dir: xdg("XDG_CACHE_HOME", ".cache").join("hyprgallery"),
            socket: socket_path(),
            log_file: state.join("daemon.log"),
        }
    }

    /// All of them under one root. For tests.
    pub fn under(root: &Path) -> Self {
        Self {
            config_file: root.join("config/config.toml"),
            style_file: root.join("config/style.css"),
            state_file: root.join("state/state.json"),
            cache_dir: root.join("cache"),
            socket: root.join("hyprgallery.sock"),
            log_file: root.join("state/daemon.log"),
        }
    }
}

/// The control socket.
///
/// `HYPRGALLERY_SOCKET` overrides it, which is how the end-to-end tests run a
/// daemon of their own next to a real one without the two meeting.
pub fn socket_path() -> PathBuf {
    if let Some(path) = env::var_os("HYPRGALLERY_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    runtime.join("hyprgallery.sock")
}

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        // The spec says relative values are invalid and must be ignored.
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => home().join(fallback),
    }
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// The user's Pictures directory, as `xdg-user-dirs` has it.
///
/// Read from `user-dirs.dirs` directly rather than by running `xdg-user-dir`,
/// which is not installed everywhere and costs a process for one line of text.
pub fn pictures_dir() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_PICTURES_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let dirs_file = config_home().join("user-dirs.dirs");
    if let Ok(text) = std::fs::read_to_string(dirs_file)
        && let Some(dir) = parse_user_dir(&text, "XDG_PICTURES_DIR")
    {
        return dir;
    }
    home().join("Pictures")
}

fn parse_user_dir(text: &str, key: &str) -> Option<PathBuf> {
    text.lines().map(str::trim).filter(|l| !l.starts_with('#')).find_map(|line| {
        let value = line.strip_prefix(key)?.trim_start().strip_prefix('=')?;
        let value = value.trim().trim_matches('"');
        Some(expand(value))
    })
}

/// Expand a leading `~` and `$HOME`, which is all a config file needs.
pub fn expand(path: &str) -> PathBuf {
    let home = home();
    if path == "~" {
        return home;
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    if let Some(rest) = path.strip_prefix("$HOME") {
        return PathBuf::from(format!("{}{rest}", home.display()));
    }
    PathBuf::from(path)
}

/// Is `name` an executable somewhere on PATH?
pub fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| {
        candidate
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// Write a file so that a reader never sees half of it.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pictures_from_user_dirs() {
        let text =
            "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_PICTURES_DIR=\"$HOME/Images\"\n";
        assert_eq!(parse_user_dir(text, "XDG_PICTURES_DIR"), Some(home().join("Images")));
        assert_eq!(parse_user_dir(text, "XDG_MUSIC_DIR"), None);
    }

    #[test]
    fn expands_home() {
        assert_eq!(expand("~/Pictures"), home().join("Pictures"));
        assert_eq!(expand("~"), home());
        assert_eq!(expand("/srv/walls"), PathBuf::from("/srv/walls"));
    }
}
