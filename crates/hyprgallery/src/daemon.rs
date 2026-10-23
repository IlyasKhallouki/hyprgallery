//! The long-running process: the engine, the gallery window, the control
//! socket, the rotation timer and the file watcher, all on GTK's main loop.

use crate::server::{self, Incoming};
use anyhow::{Context, Result};
use gtk::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;
use hyprgallery_core::engine::{Change, Engine, Event};
use hyprgallery_core::ipc::{self, Reply, Request, RotateRequest, Status};
use hyprgallery_core::paths::Paths;
use hyprgallery_core::rotation::Rotation;
use hyprgallery_core::{Layout, now};
use hyprgallery_ui::{Dispatch, Gallery};
use notify::Watcher;
use std::cell::{OnceCell, RefCell};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

pub enum Mode {
    Daemon { restore: bool },
    Snapshot(Snapshot),
}

pub struct Snapshot {
    pub out: PathBuf,
    pub layout: Layout,
    pub view: Option<String>,
    pub select: Option<u32>,
    pub peek: bool,
}

/// Everything the main loop owns.
struct Daemon {
    app: gtk::Application,
    engine: Rc<RefCell<Engine>>,
    gallery: OnceCell<Rc<Gallery>>,
    subscribers: RefCell<Vec<UnixStream>>,
    last_status: RefCell<String>,
    /// `set` requests waiting for their wallpaper to be applied.
    pending_sets: RefCell<Vec<(PathBuf, std::sync::mpsc::Sender<Reply>)>>,
    watcher: RefCell<Option<notify::RecommendedWatcher>>,
    rescan_soon: Rc<RefCell<Option<glib::SourceId>>>,
    paths: Paths,
}

pub fn run(mode: Mode) -> Result<()> {
    let mut paths = Paths::from_env();
    let mut _lock = None;
    let mut listener = None;
    let mut snapshot_dir = None;
    match &mode {
        Mode::Daemon { .. } => {
            let Some(lock) = server::lock(&paths.socket)? else {
                eprintln!("hyprgallery is already running");
                return Ok(());
            };
            _lock = Some(lock);
            listener = Some(
                server::bind(&paths.socket)
                    .with_context(|| format!("binding {}", paths.socket.display()))?,
            );
        }
        Mode::Snapshot(_) => {
            // Work on a copy of the state, so scripting a screenshot never
            // changes the real gallery's remembered album.
            let dir =
                std::env::temp_dir().join(format!("hyprgallery-snapshot-{}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            let copy = dir.join("state.json");
            let _ = std::fs::copy(&paths.state_file, &copy);
            paths.state_file = copy;
            snapshot_dir = Some(dir);
        }
    }

    // Wayland only. Under X11 there is no layer shell, and the overlay would be
    // an ordinary window that the window manager places wherever it likes.
    let app = gtk::Application::builder()
        .application_id("io.github.IlyasKhallouki.hyprgallery")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let mode = Rc::new(RefCell::new(Some(mode)));
    let listener = RefCell::new(listener);
    let paths_for_startup = paths.clone();
    app.connect_startup(move |app| {
        let Some(mode) = mode.borrow_mut().take() else { return };
        // The app has no window until asked; without a hold, GTK would quit
        // as soon as startup returned.
        std::mem::forget(app.hold());
        if let Err(e) = start(app, paths_for_startup.clone(), mode, listener.borrow_mut().take()) {
            eprintln!("hyprgallery: {e:#}");
            app.quit();
        }
    });
    app.connect_activate(|_| {});
    let code = app.run_with_args::<&str>(&[]);

    if let Some(dir) = snapshot_dir {
        let _ = std::fs::remove_dir_all(dir);
    } else {
        let _ = std::fs::remove_file(&paths.socket);
    }
    if code != glib::ExitCode::SUCCESS {
        anyhow::bail!("exited with {code:?}");
    }
    Ok(())
}

fn start(
    app: &gtk::Application,
    paths: Paths,
    mode: Mode,
    listener: Option<std::os::unix::net::UnixListener>,
) -> Result<()> {
    let (events_tx, events_rx) = async_channel::unbounded::<Event>();
    let engine = Rc::new(RefCell::new(Engine::new(paths.clone(), events_tx)));
    if let Some(e) = engine.borrow().config_error() {
        tracing::warn!("{e}; using defaults until it is fixed");
    }
    match engine.borrow().backend() {
        Ok(b) => tracing::info!("applying wallpapers with {}", b.name()),
        Err(e) => tracing::warn!("{e}"),
    }

    let daemon = Rc::new(Daemon {
        app: app.clone(),
        engine: Rc::clone(&engine),
        gallery: OnceCell::new(),
        subscribers: RefCell::new(Vec::new()),
        last_status: RefCell::new(String::new()),
        pending_sets: RefCell::new(Vec::new()),
        watcher: RefCell::new(None),
        rescan_soon: Rc::new(RefCell::new(None)),
        paths,
    });

    let weak = Rc::downgrade(&daemon);
    let dispatch: Dispatch = Rc::new(move |changes: Vec<Change>| {
        if let Some(d) = weak.upgrade() {
            d.dispatch(changes);
        }
    });
    let gallery = Gallery::build(app, Rc::clone(&engine), dispatch);
    let _ = daemon.gallery.set(gallery);

    // Results from worker threads.
    let weak = Rc::downgrade(&daemon);
    glib::spawn_future_local(async move {
        while let Ok(event) = events_rx.recv().await {
            let Some(d) = weak.upgrade() else { break };
            d.event(event);
        }
    });

    match mode {
        Mode::Daemon { restore } => {
            if let Some(listener) = listener {
                let (tx, rx) = async_channel::unbounded::<Incoming>();
                server::spawn(listener, tx);
                let weak = Rc::downgrade(&daemon);
                glib::spawn_future_local(async move {
                    while let Ok(incoming) = rx.recv().await {
                        let Some(d) = weak.upgrade() else { break };
                        d.request(incoming);
                    }
                });
            }
            let weak = Rc::downgrade(&daemon);
            glib::timeout_add_seconds_local(5, move || {
                let Some(d) = weak.upgrade() else { return glib::ControlFlow::Break };
                let changes = d.engine.borrow_mut().tick(now());
                if !changes.is_empty() {
                    d.dispatch(changes);
                }
                glib::ControlFlow::Continue
            });
            daemon.watch();
            // After the first pass of the main loop, so the socket is already
            // answering by the time this blocks on the graphics driver.
            let weak = Rc::downgrade(&daemon);
            glib::idle_add_local_once(move || {
                if let Some(d) = weak.upgrade()
                    && !d.gallery().is_shown()
                {
                    d.gallery().warm_up();
                }
            });
            for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                let weak = Rc::downgrade(&daemon);
                glib_unix::unix_signal_add_local(signal, move || {
                    if let Some(d) = weak.upgrade() {
                        d.shutdown();
                    }
                    glib::ControlFlow::Break
                });
            }
            let restore = restore && engine.borrow().config.restore_on_start;
            if restore {
                let changes = engine.borrow_mut().restore();
                daemon.dispatch(changes);
            }
            tracing::info!(
                "ready: {} albums, {} wallpapers",
                engine.borrow().library.albums.len(),
                engine.borrow().library.wallpapers.len()
            );
        }
        Mode::Snapshot(shot) => snapshot(&daemon, shot),
    }
    // Keep the daemon alive for as long as the main loop runs.
    std::mem::forget(daemon);
    Ok(())
}

impl Daemon {
    fn gallery(&self) -> &Rc<Gallery> {
        self.gallery.get().expect("gallery is built before anything is dispatched")
    }

    fn dispatch(&self, changes: Vec<Change>) {
        if let Some(gallery) = self.gallery.get() {
            gallery.on_changes(&changes);
        }
        self.broadcast();
    }

    fn status(&self) -> Status {
        self.engine.borrow().status()
    }

    /// Tell every bar, but only when something they show has changed.
    fn broadcast(&self) {
        let status = self.status();
        let Ok(line) = serde_json::to_string(&status) else { return };
        if *self.last_status.borrow() == line {
            return;
        }
        self.last_status.replace(line);
        self.subscribers.borrow_mut().retain(|stream| ipc::write_line(stream, &status).is_ok());
    }

    fn event(&self, event: Event) {
        if let Event::Applied { path, result } = &event {
            let mut pending = self.pending_sets.borrow_mut();
            let (done, rest): (Vec<_>, Vec<_>) = pending.drain(..).partition(|(p, _)| p == path);
            *pending = rest;
            drop(pending);
            for (_, reply) in done {
                let _ = reply.send(match result {
                    Ok(()) => Reply::ok(),
                    Err(e) => Reply::error(e.clone()),
                });
            }
        }
        let changes = self.engine.borrow_mut().handle(event);
        self.dispatch(changes);
    }

    fn request(self: &Rc<Self>, incoming: Incoming) {
        let Incoming { request, reply, stream } = incoming;
        let answer = |r: Reply| {
            let _ = reply.send(r);
        };
        match request {
            Request::Subscribe => {
                if let Some(stream) = stream
                    && ipc::write_line(&stream, &self.status()).is_ok()
                {
                    self.subscribers.borrow_mut().push(stream);
                }
            }
            Request::Toggle { layout } => {
                self.gallery().toggle(layout);
                answer(Reply::ok());
            }
            Request::Show { layout } => {
                self.gallery().show(layout);
                answer(Reply::ok());
            }
            Request::Hide => {
                self.gallery().hide();
                answer(Reply::ok());
            }
            Request::Set { path } => {
                if !path.is_file() {
                    return answer(Reply::error(format!("{} is not a file", path.display())));
                }
                let changes = {
                    let mut engine = self.engine.borrow_mut();
                    let transition = engine.transition(None);
                    engine.apply(&path, Some(transition))
                };
                if let Some(Change::Error(e)) =
                    changes.iter().find(|c| matches!(c, Change::Error(_)))
                {
                    answer(Reply::error(e.clone()));
                } else {
                    self.pending_sets.borrow_mut().push((path, reply));
                }
                self.dispatch(changes);
            }
            // Each result is bound before it is handled, so the engine borrow
            // has ended by the time the changes go out to the gallery, which
            // borrows it again.
            Request::Next { album } => {
                let result = self.engine.borrow_mut().step(album.as_deref(), 1);
                self.answer_changes(result, answer);
            }
            Request::Prev { album } => {
                let result = self.engine.borrow_mut().step(album.as_deref(), -1);
                self.answer_changes(result, answer);
            }
            Request::Random { album } => {
                let result = self.engine.borrow_mut().random(album.as_deref());
                self.answer_changes(result, answer);
            }
            Request::Rotate(RotateRequest::On { every_secs, album, shuffle }) => {
                let view = self.engine.borrow().view_for(album.as_deref());
                match view {
                    Ok(view) => {
                        let changes = self.engine.borrow_mut().set_rotation(Some(Rotation::new(
                            view,
                            every_secs,
                            shuffle,
                            now(),
                        )));
                        self.dispatch(changes);
                        answer(Reply::status(self.status()));
                    }
                    Err(e) => answer(Reply::error(e)),
                }
            }
            Request::Rotate(RotateRequest::Off) => {
                let changes = self.engine.borrow_mut().set_rotation(None);
                self.dispatch(changes);
                answer(Reply::status(self.status()));
            }
            Request::Restore => {
                let changes = self.engine.borrow_mut().restore();
                self.dispatch(changes);
                answer(Reply::ok());
            }
            Request::Reindex => {
                let changes = self.engine.borrow_mut().rescan();
                self.dispatch(changes);
                answer(Reply::ok());
            }
            Request::Status => answer(Reply::status(self.status())),
            Request::Quit => {
                answer(Reply::ok());
                self.shutdown();
            }
        }
    }

    fn answer_changes(&self, result: Result<Vec<Change>, String>, answer: impl FnOnce(Reply)) {
        match result {
            Ok(changes) => {
                self.dispatch(changes);
                answer(Reply::ok());
            }
            Err(e) => answer(Reply::error(e)),
        }
    }

    fn shutdown(&self) {
        tracing::info!("shutting down");
        self.engine.borrow_mut().persist();
        let _ = std::fs::remove_file(&self.paths.socket);
        self.app.quit();
    }

    /// Watch the library, the config file and the stylesheet.
    fn watch(self: &Rc<Self>) {
        let (tx, rx) = async_channel::unbounded::<notify::Event>();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                let _ = tx.send_blocking(event);
            }
        });
        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("cannot watch for changes: {e}");
                return;
            }
        };
        let (roots, hyde) = {
            let engine = self.engine.borrow();
            let roots = engine.config.library.root_paths();
            let hyde = engine
                .library
                .albums
                .iter()
                .filter(|a| a.origin == hyprgallery_core::library::Origin::Hyde)
                .map(|a| a.dir.clone())
                .collect::<Vec<_>>();
            (roots, hyde)
        };
        for dir in roots.iter().chain(hyde.iter()) {
            if let Err(e) = watcher.watch(dir, notify::RecursiveMode::Recursive) {
                tracing::debug!("not watching {}: {e}", dir.display());
            }
        }
        if let Some(config_dir) = self.paths.config_file.parent() {
            let _ = std::fs::create_dir_all(config_dir);
            let _ = watcher.watch(config_dir, notify::RecursiveMode::NonRecursive);
        }
        self.watcher.replace(Some(watcher));

        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                let Some(d) = weak.upgrade() else { break };
                d.file_event(event);
            }
        });
    }

    fn file_event(self: &Rc<Self>, event: notify::Event) {
        use notify::EventKind;
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let touches = |p: &Path| event.paths.iter().any(|e| e == p);
        if touches(&self.paths.config_file) {
            let changes = self.engine.borrow_mut().reload_config();
            let roots_changed =
                changes.contains(&Change::Library) || changes.contains(&Change::Indexing);
            self.dispatch(changes);
            if roots_changed {
                self.watch();
            }
            return;
        }
        if touches(&self.paths.style_file) {
            self.gallery().reload_style();
            return;
        }
        if event.paths.iter().any(|p| p.starts_with(&self.paths.cache_dir) || is_partial(p)) {
            return;
        }
        // Copying a folder in arrives as hundreds of events. Wait for quiet.
        if let Some(id) = self.rescan_soon.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let slot = Rc::clone(&self.rescan_soon);
        let id = glib::timeout_add_local_once(Duration::from_millis(600), move || {
            slot.borrow_mut().take();
            if let Some(d) = weak.upgrade() {
                let changes = d.engine.borrow_mut().rescan();
                d.dispatch(changes);
            }
        });
        *self.rescan_soon.borrow_mut() = Some(id);
    }
}

/// Files a browser or copy is still writing.
fn is_partial(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    name.ends_with(".part")
        || name.ends_with(".crdownload")
        || name.ends_with(".tmp")
        || name.contains(".tmp.")
}

/// Open the gallery, wait for every visible thumbnail, then save a PNG.
fn snapshot(daemon: &Rc<Daemon>, shot: Snapshot) {
    let gallery = Rc::clone(daemon.gallery());
    if let Some(view) = &shot.view
        && !gallery.set_view_named(view)
    {
        eprintln!("hyprgallery: no album called `{view}`");
    }
    gallery.show(Some(shot.layout));
    gallery.window.fullscreen();
    let weak: Weak<Daemon> = Rc::downgrade(daemon);
    let started = std::time::Instant::now();
    let stage = Rc::new(std::cell::Cell::new(0u8));
    glib::timeout_add_local(Duration::from_millis(250), move || {
        let Some(d) = weak.upgrade() else { return glib::ControlFlow::Break };
        let g = Rc::clone(d.gallery());
        let indexing = d.engine.borrow().status().indexing.is_some();
        let timed_out = started.elapsed() > Duration::from_secs(600);
        match stage.get() {
            0 if (!indexing
                && !g.thumbnails_pending()
                && started.elapsed() > Duration::from_millis(1500))
                || timed_out =>
            {
                if let Some(pos) = shot.select {
                    g.select_index(pos);
                }
                if shot.peek {
                    g.start_peek();
                }
                stage.set(1);
            }
            1 => stage.set(2),
            2 => stage.set(3),
            3 => {
                match g.save_png(&shot.out) {
                    Ok(()) => println!("{}", shot.out.display()),
                    Err(e) => eprintln!("hyprgallery: {e:#}"),
                }
                d.engine.borrow_mut().persist();
                d.app.quit();
                return glib::ControlFlow::Break;
            }
            _ => {}
        }
        glib::ControlFlow::Continue
    });
}
