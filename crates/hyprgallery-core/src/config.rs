//! `config.toml`. Every key is optional and the file itself is optional.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub library: LibraryConfig,
    /// What `toggle` opens when it is not told.
    pub layout: Layout,
    pub backend: BackendChoice,
    /// Shell command for `backend = "command"`. `{path}` becomes the file.
    pub command: Option<String>,
    /// Close the gallery after choosing a wallpaper.
    pub close_on_apply: bool,
    /// Put the last wallpaper back when the daemon starts. Ignored on HyDE,
    /// which restores its own.
    pub restore_on_start: bool,
    pub transition: Transition,
    pub popover: PopoverConfig,
    pub bar: BarConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            library: LibraryConfig::default(),
            layout: Layout::Overlay,
            backend: BackendChoice::Auto,
            command: None,
            close_on_apply: true,
            restore_on_start: true,
            transition: Transition::default(),
            popover: PopoverConfig::default(),
            bar: BarConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LibraryConfig {
    /// Folders to read. Each subfolder becomes an album.
    pub roots: Vec<String>,
    /// Also show each HyDE theme's wallpapers as an album, when HyDE is there.
    pub hyde_themes: bool,
    /// How many files "Recently added" holds.
    pub recent: usize,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self { roots: vec![default_root()], hyde_themes: true, recent: 48 }
    }
}

fn default_root() -> String {
    let pictures = crate::paths::pictures_dir().join("Wallpapers");
    let home = crate::paths::home();
    match pictures.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => pictures.display().to_string(),
    }
}

impl LibraryConfig {
    pub fn root_paths(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|r| crate::paths::expand(r)).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// Full screen, blurred backdrop of whatever is focused.
    #[default]
    Overlay,
    /// A panel hanging from the bar.
    Popover,
}

impl std::str::FromStr for Layout {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "overlay" => Ok(Self::Overlay),
            "popover" => Ok(Self::Popover),
            other => Err(format!("unknown layout `{other}`, expected overlay or popover")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendChoice {
    #[default]
    Auto,
    Hyde,
    Awww,
    Swww,
    Hyprpaper,
    Swaybg,
    Command,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Transition {
    /// Any awww/swww transition type. `grow` starts from where you clicked.
    #[serde(rename = "type")]
    pub kind: String,
    pub duration: f32,
    pub fps: u32,
}

impl Default for Transition {
    fn default() -> Self {
        Self { kind: "grow".into(), duration: 0.9, fps: 60 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    TopLeft,
    Top,
    #[default]
    TopRight,
    BottomLeft,
    Bottom,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PopoverConfig {
    pub anchor: Anchor,
    /// Gap from the anchored edges, in logical pixels.
    pub margin: i32,
    pub width: i32,
    pub height: i32,
}

impl Default for PopoverConfig {
    fn default() -> Self {
        Self { anchor: Anchor::TopRight, margin: 8, width: 820, height: 600 }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BarConfig {
    /// Text for the Waybar module. Defaults are Nerd Font glyphs.
    pub icon: String,
    pub rotating_icon: String,
}

impl Default for BarConfig {
    fn default() -> Self {
        // nf-md-wallpaper and nf-md-autorenew.
        Self { icon: "\u{F0E09}".into(), rotating_icon: "\u{F006A}".into() }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
}

impl Config {
    /// Load the file, or the defaults when there is none.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .map_err(|message| ConfigError::Parse { path: path.to_owned(), message }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read { path: path.to_owned(), source }),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string().trim().to_owned())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.backend == BackendChoice::Command
            && self.command.as_deref().is_none_or(|c| c.trim().is_empty())
        {
            return Err("backend = \"command\" needs a `command` to run".into());
        }
        if !(0.0..=10.0).contains(&self.transition.duration) {
            return Err("transition.duration is in seconds and must be between 0 and 10".into());
        }
        if self.popover.width < 320 || self.popover.height < 240 {
            return Err("popover is too small to be usable (minimum 320x240)".into());
        }
        Ok(())
    }

    /// Write the commented starter file if there is no config yet, so the
    /// options are discoverable from the file itself.
    pub fn write_template_if_missing(path: &Path) -> std::io::Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        let text = TEMPLATE.replace("{root}", &default_root());
        crate::paths::write_atomic(path, text.as_bytes())?;
        Ok(true)
    }
}

/// The starter config. Everything is commented out, so it changes nothing
/// until edited, and each default is visible next to its key.
pub const TEMPLATE: &str = r#"# hyprgallery configuration. Every key is optional; the values shown are the
# defaults. The daemon reloads this file when it changes.

[library]
# Folders to read. Each subfolder becomes an album; images directly inside a
# root form an album named after the root.
# roots = ["{root}"]

# On HyDE, also show each theme's wallpapers as an album.
# hyde_themes = true

# How many of the newest files the "Recent" album holds.
# recent = 48

# What `hyprgallery toggle` opens: "overlay" (full screen) or "popover".
# layout = "overlay"

# How wallpapers are applied: "auto", "hyde", "awww", "swww", "hyprpaper",
# "swaybg" or "command". Auto prefers HyDE, then awww, swww, hyprpaper, swaybg.
# backend = "auto"

# For backend = "command". {path} is replaced with the file, safely quoted.
# command = "my-wallpaper-script {path}"

# close_on_apply = true
# restore_on_start = true

[transition]
# Used by awww and swww. "grow" expands from the thumbnail you picked.
# type = "grow"
# duration = 0.9
# fps = 60

[popover]
# anchor = "top-right"   # top-left, top, top-right, bottom-left, bottom, bottom-right
# margin = 8
# width = 820
# height = 600

[bar]
# Text Waybar shows. The defaults are Nerd Font glyphs.
# icon = "..."
# rotating_icon = "..."
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn template_parses_to_defaults() {
        let text = TEMPLATE.replace("{root}", "~/Pictures/Wallpapers");
        assert_eq!(Config::parse(&text).unwrap(), Config::default());
    }

    #[test]
    fn reads_overrides() {
        let config = Config::parse(
            r#"
            layout = "popover"
            backend = "swww"
            [library]
            roots = ["~/walls", "/srv/art"]
            hyde_themes = false
            [transition]
            type = "wipe"
            [popover]
            anchor = "bottom-left"
            "#,
        )
        .unwrap();
        assert_eq!(config.layout, Layout::Popover);
        assert_eq!(config.backend, BackendChoice::Swww);
        assert_eq!(config.library.roots, ["~/walls", "/srv/art"]);
        assert!(!config.library.hyde_themes);
        assert_eq!(config.transition.kind, "wipe");
        assert_eq!(config.transition.fps, 60);
        assert_eq!(config.popover.anchor, Anchor::BottomLeft);
    }

    #[test]
    fn rejects_typos_with_a_location() {
        let err = Config::parse("[library]\nroot = [\"~/x\"]\n").unwrap_err();
        assert!(err.contains("root"), "{err}");
    }

    #[test]
    fn command_backend_needs_a_command() {
        assert!(Config::parse("backend = \"command\"").is_err());
        assert!(Config::parse("backend = \"command\"\ncommand = \"x {path}\"").is_ok());
    }

    #[test]
    fn missing_file_means_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(&dir.path().join("nope.toml")).unwrap(), Config::default());
    }
}
