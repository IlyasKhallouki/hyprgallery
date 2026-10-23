//! What hyprgallery remembers between runs: the current wallpaper, your
//! favourites, the rotation, and where you were in the gallery.

use crate::rotation::Rotation;
use crate::view::ViewId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many applied wallpapers to remember, for shuffling without repeats.
const HISTORY: usize = 32;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub current: Option<PathBuf>,
    /// Newest first.
    pub favourites: Vec<PathBuf>,
    pub rotation: Option<Rotation>,
    /// Newest first.
    pub history: Vec<PathBuf>,
    /// The album the gallery last showed.
    pub view: Option<ViewId>,
}

impl State {
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("reading {}: {e}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(e) => {
                // Keep the broken file for inspection rather than silently
                // overwriting someone's favourites on the next save.
                let backup = path.with_extension("json.broken");
                tracing::warn!("{}: {e}; moved aside to {}", path.display(), backup.display());
                let _ = std::fs::rename(path, backup);
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::paths::write_atomic(path, &bytes)
    }

    pub fn is_favourite(&self, path: &Path) -> bool {
        self.favourites.iter().any(|f| f == path)
    }

    /// Returns whether it is a favourite now.
    pub fn toggle_favourite(&mut self, path: &Path) -> bool {
        if let Some(i) = self.favourites.iter().position(|f| f == path) {
            self.favourites.remove(i);
            false
        } else {
            self.favourites.insert(0, path.to_owned());
            true
        }
    }

    pub fn set_current(&mut self, path: &Path) {
        self.current = Some(path.to_owned());
        self.history.retain(|h| h != path);
        self.history.insert(0, path.to_owned());
        self.history.truncate(HISTORY);
    }

    /// A file moved. Follow it everywhere it is remembered.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        let swap = |p: &mut PathBuf| {
            if p == from {
                *p = to.to_owned();
            }
        };
        self.current.iter_mut().for_each(swap);
        self.favourites.iter_mut().for_each(swap);
        self.history.iter_mut().for_each(swap);
    }

    /// A file is gone. Forget it, except as the current wallpaper, which is
    /// still what is on screen until something else is chosen.
    pub fn forget(&mut self, path: &Path) {
        self.favourites.retain(|p| p != path);
        self.history.retain(|p| p != path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favourites_toggle_newest_first() {
        let mut s = State::default();
        assert!(s.toggle_favourite(Path::new("/a")));
        assert!(s.toggle_favourite(Path::new("/b")));
        assert_eq!(s.favourites, [PathBuf::from("/b"), PathBuf::from("/a")]);
        assert!(!s.toggle_favourite(Path::new("/b")));
        assert!(s.is_favourite(Path::new("/a")) && !s.is_favourite(Path::new("/b")));
    }

    #[test]
    fn history_has_no_duplicates_and_is_bounded() {
        let mut s = State::default();
        for i in 0..40 {
            s.set_current(Path::new(&format!("/w{i}")));
        }
        s.set_current(Path::new("/w39"));
        s.set_current(Path::new("/w20"));
        assert_eq!(s.history.len(), HISTORY);
        assert_eq!(s.history[0], PathBuf::from("/w20"));
        assert_eq!(s.history[1], PathBuf::from("/w39"));
    }

    #[test]
    fn rename_follows_the_file() {
        let mut s = State::default();
        s.set_current(Path::new("/old"));
        s.toggle_favourite(Path::new("/old"));
        s.rename(Path::new("/old"), Path::new("/new"));
        assert_eq!(s.current.as_deref(), Some(Path::new("/new")));
        assert!(s.is_favourite(Path::new("/new")));
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(State::load(&path), State::default());
        assert!(dir.path().join("state.json.broken").exists());
    }

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.json");
        let mut s = State::default();
        s.set_current(Path::new("/x.jpg"));
        s.view = Some(ViewId::Favourites);
        s.save(&path).unwrap();
        assert_eq!(State::load(&path), s);
    }
}
