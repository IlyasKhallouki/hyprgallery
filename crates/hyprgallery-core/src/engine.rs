//! The daemon's state and every operation on it, without any drawing.
//!
//! The engine lives on one thread (the GTK main thread, in the daemon) and is
//! never shared. Slow work, decoding and running wallpaper programs, goes to
//! worker threads, whose results come back as [`Event`]s on a channel for the
//! owner to feed into [`Engine::handle`]. Every mutation returns the
//! [`Change`]s it caused, so the UI and the bar update from one place.

use crate::backend::{self, Backend, Probe as _, Transition};
use crate::config::Config;
use crate::ipc::{RotationStatus, Status};
use crate::library::{Kind, Library, Sources};
use crate::paths::Paths;
use crate::rotation::{self, Rotation};
use crate::state::State;
use crate::thumbs::{self, Meta, MetaStore};
use crate::view::{self, ViewId};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Results from worker threads.
#[derive(Debug)]
pub enum Event {
    Thumb { key: String, result: Result<Meta, String> },
    Applied { path: PathBuf, result: Result<(), String> },
    Imported { result: Result<Vec<PathBuf>, String> },
}

/// What changed, for whoever draws it.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Albums or files were added, removed or moved.
    Library,
    /// A thumbnail and its colours are ready.
    Meta(String),
    Favourite(PathBuf),
    Current,
    Applying,
    Rotation,
    Indexing,
    Config,
    /// Something the user should hear about.
    Error(String),
    /// Something the user should hear about that is not a problem.
    Notice(String),
}

pub struct Engine {
    pub paths: Paths,
    pub config: Config,
    config_error: Option<String>,
    pub library: Library,
    pub metas: MetaStore,
    pub state: State,
    backend: Result<Backend, String>,
    events: async_channel::Sender<Event>,
    pool: rayon::ThreadPool,
    pending: HashSet<String>,
    failed: HashSet<String>,
    indexing: Option<(usize, usize)>,
    applying: Option<PathBuf>,
    /// The most recent request made while another was still applying. Rapid
    /// clicks collapse into this one slot, so only the last choice runs.
    queued: Option<(PathBuf, Option<Transition>)>,
    last_error: Option<String>,
    /// Whether the gallery is on screen. Reported to the bar.
    pub visible: bool,
}

impl Engine {
    pub fn new(paths: Paths, events: async_channel::Sender<Event>) -> Self {
        if let Err(e) = Config::write_template_if_missing(&paths.config_file) {
            tracing::warn!("could not write the starter config: {e}");
        }
        let (config, config_error) = match Config::load(&paths.config_file) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        };
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("hg-thumb-{i}"))
            // Thumbnailing a new folder should never make the desktop stutter.
            .start_handler(|_| unsafe {
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            })
            .build()
            .expect("thread pool");
        let mut engine = Self {
            state: State::load(&paths.state_file),
            metas: MetaStore::load(&paths.cache_dir),
            backend: backend::detect(&config, &backend::System).map_err(|e| e.to_string()),
            paths,
            config,
            config_error,
            library: Library::default(),
            events,
            pool,
            pending: HashSet::new(),
            failed: HashSet::new(),
            indexing: None,
            applying: None,
            queued: None,
            last_error: None,
            visible: false,
        };
        engine.rescan();
        engine
    }

    pub fn backend(&self) -> Result<&Backend, &str> {
        self.backend.as_ref().map_err(String::as_str)
    }

    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    pub fn is_applying(&self, path: &Path) -> bool {
        self.applying.as_deref() == Some(path)
    }

    pub fn failed(&self, key: &str) -> bool {
        self.failed.contains(key)
    }

    pub fn thumb_path(&self, key: &str) -> PathBuf {
        thumbs::thumb_path(&self.paths.cache_dir, key)
    }

    fn sources(&self) -> Sources {
        let hyde = self.config.library.hyde_themes && backend::System.hyde();
        Sources {
            roots: self.config.library.root_paths(),
            hyde_themes: hyde.then(|| crate::paths::config_home().join("hyde/themes")),
        }
    }

    /// Read the folders again and thumbnail anything new.
    pub fn rescan(&mut self) -> Vec<Change> {
        let library = Library::scan(&self.sources());
        let changed = library != self.library;
        self.library = library;
        let live: HashSet<&str> = self.library.wallpapers.iter().map(|w| w.key.as_str()).collect();
        // Only prune once something has been scanned, so a root on a drive
        // that is not mounted yet does not wipe the whole cache.
        if !live.is_empty() {
            self.metas.prune(&self.paths.cache_dir, &live);
        }
        self.queue_thumbs();
        let mut changes = vec![Change::Indexing];
        if changed {
            changes.push(Change::Library);
        }
        changes
    }

    fn queue_thumbs(&mut self) {
        let current_album = self.current_index().map(|i| self.library.wallpapers[i].album);
        let mut todo: Vec<usize> = (0..self.library.wallpapers.len())
            .filter(|&i| {
                let key = &self.library.wallpapers[i].key;
                !self.pending.contains(key)
                    && !self.failed.contains(key)
                    && !self.metas.ready(&self.paths.cache_dir, key)
            })
            .collect();
        if todo.is_empty() {
            return;
        }
        // The album you are most likely to open first gets done first.
        todo.sort_by_key(|&i| Some(self.library.wallpapers[i].album) != current_album);
        let (done, total) = self.indexing.unwrap_or((0, 0));
        self.indexing = Some((done, total + todo.len()));
        for i in todo {
            let wallpaper = self.library.wallpapers[i].clone();
            self.pending.insert(wallpaper.key.clone());
            let cache = self.paths.cache_dir.clone();
            let events = self.events.clone();
            self.pool.spawn_fifo(move || {
                let result = thumbs::generate(&wallpaper, &cache);
                let _ = events.send_blocking(Event::Thumb { key: wallpaper.key, result });
            });
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Change> {
        match event {
            Event::Thumb { key, result } => {
                self.pending.remove(&key);
                match result {
                    Ok(meta) => self.metas.insert(key.clone(), meta),
                    Err(e) => {
                        tracing::warn!("thumbnail {key}: {e}");
                        self.failed.insert(key.clone());
                    }
                }
                let mut changes = vec![Change::Meta(key)];
                if let Some((done, _)) = &mut self.indexing {
                    *done += 1;
                    // Persist as it goes, so quitting mid-way keeps the work.
                    if self.pending.is_empty() || *done % 32 == 0 {
                        let _ = self.metas.save(&self.paths.cache_dir);
                    }
                    if self.pending.is_empty() {
                        self.indexing = None;
                        // Counts in smart albums depend on what was learned.
                        changes.push(Change::Library);
                    }
                    changes.push(Change::Indexing);
                }
                changes
            }
            Event::Applied { path, result } => {
                self.applying = None;
                let mut changes = vec![Change::Applying];
                match result {
                    Ok(()) => {
                        self.state.set_current(&path);
                        self.last_error = None;
                        self.save_state();
                        changes.push(Change::Current);
                    }
                    Err(e) => {
                        tracing::warn!("applying {}: {e}", path.display());
                        self.last_error = Some(e.clone());
                        changes.push(Change::Error(e));
                    }
                }
                if let Some((next, transition)) = self.queued.take() {
                    changes.extend(self.apply(&next, transition));
                }
                changes
            }
            Event::Imported { result } => match result {
                Ok(files) => {
                    let mut changes = self.rescan();
                    let n = files.len();
                    changes.push(Change::Notice(match n {
                        1 => "Added 1 wallpaper".into(),
                        n => format!("Added {n} wallpapers"),
                    }));
                    changes
                }
                Err(e) => vec![Change::Error(e)],
            },
        }
    }

    /// Put `path` on screen. Returns at once; the result arrives as an event.
    pub fn apply(&mut self, path: &Path, transition: Option<Transition>) -> Vec<Change> {
        if self.applying.is_some() {
            self.queued = Some((path.to_owned(), transition));
            return Vec::new();
        }
        let backend = match &self.backend {
            Ok(b) => b.clone(),
            Err(e) => return vec![Change::Error(e.clone())],
        };
        let kind = self
            .library
            .find(path)
            .map(|i| self.library.wallpapers[i].kind)
            .or_else(|| Kind::of(path))
            .unwrap_or(Kind::Still);
        let steps = match backend::plan(&backend, path, kind, transition.as_ref()) {
            Ok(steps) => steps,
            Err(e) => return vec![Change::Error(e.to_string())],
        };
        self.applying = Some(path.to_owned());
        let events = self.events.clone();
        let path = path.to_owned();
        std::thread::Builder::new()
            .name("hg-apply".into())
            .spawn(move || {
                let result = backend::execute(&steps).map_err(|e| e.to_string());
                let _ = events.send_blocking(Event::Applied { path, result });
            })
            .expect("spawn apply thread");
        vec![Change::Applying]
    }

    /// The transition to use for a change the user asked for.
    pub fn transition(&self, at: Option<(i32, i32)>) -> Transition {
        Transition::from_config(&self.config, at)
    }

    pub fn current_index(&self) -> Option<usize> {
        self.state.current.as_deref().and_then(|p| self.library.find(p))
    }

    /// Resolve `--album`, falling back to the album of the current wallpaper.
    pub fn view_for(&self, album: Option<&str>) -> Result<ViewId, String> {
        match album {
            Some(name) => ViewId::parse(&self.library, name)
                .ok_or_else(|| format!("no album called `{name}`")),
            None => self
                .current_index()
                .map(|i| ViewId::Album(self.library.albums[self.library.wallpapers[i].album].id()))
                .or_else(|| self.state.view.clone())
                .or_else(|| self.library.albums.first().map(|a| ViewId::Album(a.id())))
                .ok_or_else(|| "the library is empty".into()),
        }
    }

    pub fn order(&self, view: &ViewId) -> Vec<usize> {
        view::resolve(&self.library, &self.state, &self.metas, view, self.config.library.recent)
    }

    pub fn views(&self) -> Vec<view::ViewInfo> {
        view::available(&self.library, &self.state, &self.metas, self.config.library.recent)
    }

    /// Next or previous wallpaper in an album.
    pub fn step(&mut self, album: Option<&str>, delta: isize) -> Result<Vec<Change>, String> {
        let order = self.order(&self.view_for(album)?);
        let next =
            rotation::step(&order, self.current_index(), delta).ok_or("that album is empty")?;
        let path = self.library.wallpapers[next].path.clone();
        let transition = self.transition(None);
        Ok(self.apply(&path, Some(transition)))
    }

    pub fn random(&mut self, album: Option<&str>) -> Result<Vec<Change>, String> {
        let order = self.order(&self.view_for(album)?);
        let paths: Vec<PathBuf> = self.library.wallpapers.iter().map(|w| w.path.clone()).collect();
        let next = rotation::pick(
            &order,
            &paths,
            self.current_index(),
            true,
            &self.state.history,
            rotation::seed(),
        )
        .ok_or("that album is empty")?;
        let transition = self.transition(None);
        Ok(self.apply(&paths[next], Some(transition)))
    }

    pub fn set_rotation(&mut self, rotation: Option<Rotation>) -> Vec<Change> {
        self.state.rotation = rotation;
        self.save_state();
        vec![Change::Rotation]
    }

    /// Called every few seconds. Changes the wallpaper when a rotation is due.
    pub fn tick(&mut self, now: i64) -> Vec<Change> {
        let Some(rot) = self.state.rotation.clone() else { return Vec::new() };
        if !rot.due(now) || self.applying.is_some() {
            return Vec::new();
        }
        let order = self.order(&rot.view);
        let paths: Vec<PathBuf> = self.library.wallpapers.iter().map(|w| w.path.clone()).collect();
        let mut changes = Vec::new();
        if let Some(next) = rotation::pick(
            &order,
            &paths,
            self.current_index(),
            rot.shuffle,
            &self.state.history,
            rotation::seed(),
        ) {
            let transition = self.transition(None);
            changes.extend(self.apply(&paths[next], Some(transition)));
        }
        if let Some(r) = &mut self.state.rotation {
            r.advance(now);
        }
        self.save_state();
        changes.push(Change::Rotation);
        changes
    }

    /// Put the last wallpaper back, unless the backend does that itself or is
    /// already showing it.
    pub fn restore(&mut self) -> Vec<Change> {
        let Ok(backend) = &self.backend else { return Vec::new() };
        let Some(current) = self.state.current.clone() else { return Vec::new() };
        if backend.restores_itself() || !current.exists() {
            return Vec::new();
        }
        if let Backend::Awww { bin } = backend
            && backend::awww_current(bin).contains(&current)
        {
            return Vec::new();
        }
        self.apply(&current, None)
    }

    pub fn reload_config(&mut self) -> Vec<Change> {
        match Config::load(&self.paths.config_file) {
            Ok(config) => {
                self.config_error = None;
                let rescan = config.library != self.config.library;
                let redetect =
                    config.backend != self.config.backend || config.command != self.config.command;
                self.config = config;
                if redetect {
                    self.backend =
                        backend::detect(&self.config, &backend::System).map_err(|e| e.to_string());
                }
                let mut changes = vec![Change::Config];
                if rescan {
                    changes.extend(self.rescan());
                }
                changes
            }
            Err(e) => {
                let message = e.to_string();
                self.config_error = Some(message.clone());
                vec![Change::Error(message)]
            }
        }
    }

    pub fn toggle_favourite(&mut self, path: &Path) -> Vec<Change> {
        self.state.toggle_favourite(path);
        self.save_state();
        vec![Change::Favourite(path.to_owned())]
    }

    pub fn set_view(&mut self, view: ViewId) {
        if self.state.view.as_ref() != Some(&view) {
            self.state.view = Some(view);
            self.save_state();
        }
    }

    /// Move a file into another album's folder.
    pub fn move_to(&mut self, path: &Path, album: usize) -> Result<Vec<Change>, String> {
        let dir = self.library.albums.get(album).ok_or("no such album")?.dir.clone();
        let target = unique_target(&dir, path)?;
        move_file(path, &target).map_err(|e| format!("could not move {}: {e}", path.display()))?;
        let was_current = self.state.current.as_deref() == Some(path);
        self.state.rename(path, &target);
        self.save_state();
        let mut changes = self.rescan();
        // HyDE keeps a symlink to the current wallpaper and falls back to its
        // theme default at login when the link breaks. Point it at the new home.
        if was_current && self.backend.as_ref().is_ok_and(|b| matches!(b, Backend::Hyde)) {
            changes.extend(self.apply(&target, None));
        }
        let name = self.library.albums.get(album).map(|a| a.name.clone()).unwrap_or_default();
        changes.push(Change::Notice(format!("Moved to {name}")));
        Ok(changes)
    }

    /// A file was sent to the trash by the UI.
    pub fn forget(&mut self, path: &Path) -> Vec<Change> {
        self.state.forget(path);
        self.save_state();
        self.rescan()
    }

    /// Copy files into an album, off the main thread. Videos can be large.
    pub fn import(&mut self, files: Vec<PathBuf>, album: Option<usize>) -> Result<(), String> {
        let dir = match album.and_then(|a| self.library.albums.get(a)) {
            Some(a) if a.origin != crate::library::Origin::Hyde => a.dir.clone(),
            _ => self
                .config
                .library
                .root_paths()
                .into_iter()
                .next()
                .ok_or("no library folder is configured")?,
        };
        let files: Vec<PathBuf> = files.into_iter().filter(|f| Kind::of(f).is_some()).collect();
        if files.is_empty() {
            return Err("none of those are images or videos".into());
        }
        let events = self.events.clone();
        std::thread::spawn(move || {
            let result = (|| {
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let mut added = Vec::new();
                for file in &files {
                    if file.parent() == Some(dir.as_path()) {
                        continue;
                    }
                    let target = unique_target(&dir, file)?;
                    std::fs::copy(file, &target).map_err(|e| format!("{}: {e}", file.display()))?;
                    added.push(target);
                }
                Ok(added)
            })();
            let _ = events.send_blocking(Event::Imported { result });
        });
        Ok(())
    }

    fn save_state(&self) {
        if let Err(e) = self.state.save(&self.paths.state_file) {
            tracing::warn!("saving state: {e}");
        }
    }

    /// Flush anything held in memory. Called on shutdown.
    pub fn persist(&mut self) {
        self.save_state();
        let _ = self.metas.save(&self.paths.cache_dir);
    }

    pub fn status(&self) -> Status {
        let current = self.state.current.clone();
        let found = self.current_index().map(|i| &self.library.wallpapers[i]);
        let name = found.map(|w| w.name.clone()).or_else(|| {
            current
                .as_deref()
                .and_then(Path::file_stem)
                .map(|s| crate::naming::display_name(&s.to_string_lossy()))
        });
        Status {
            name,
            album: found.map(|w| self.library.albums[w.album].name.clone()),
            current,
            applying: self.applying.is_some(),
            rotation: self.state.rotation.as_ref().map(|r| RotationStatus {
                album: r.view.label(&self.library),
                every_secs: r.every_secs,
                shuffle: r.shuffle,
                next_at: r.next_at,
            }),
            indexing: self.indexing,
            visible: self.visible,
            backend: self.backend.as_ref().ok().map(|b| b.name().to_owned()),
            error: self
                .last_error
                .clone()
                .or_else(|| self.backend.as_ref().err().cloned())
                .or_else(|| self.config_error.clone()),
        }
    }
}

/// `dir/name`, or `dir/name (2)` and so on when that is taken.
fn unique_target(dir: &Path, file: &Path) -> Result<PathBuf, String> {
    let name = file.file_name().ok_or("not a file")?;
    let first = dir.join(name);
    if !first.exists() {
        return Ok(first);
    }
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = file.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..1000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .ok_or_else(|| "too many files with that name".into())
}

/// Rename, or copy and delete when the target is on another filesystem.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_target_counts_up() {
        let dir = tempfile::tempdir().unwrap();
        let src = Path::new("/elsewhere/sky.jpg");
        assert_eq!(unique_target(dir.path(), src).unwrap(), dir.path().join("sky.jpg"));
        std::fs::write(dir.path().join("sky.jpg"), b"").unwrap();
        std::fs::write(dir.path().join("sky (2).jpg"), b"").unwrap();
        assert_eq!(unique_target(dir.path(), src).unwrap(), dir.path().join("sky (3).jpg"));
    }
}
