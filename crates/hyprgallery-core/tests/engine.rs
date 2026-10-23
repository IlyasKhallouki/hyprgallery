//! The engine end to end, with a backend that only writes down what it was
//! asked to apply.

use hyprgallery_core::engine::{Change, Engine, Event};
use hyprgallery_core::paths::Paths;
use hyprgallery_core::rotation::Rotation;
use hyprgallery_core::view::ViewId;
use std::path::{Path, PathBuf};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    applied: PathBuf,
    engine: Engine,
    events: async_channel::Receiver<Event>,
}

fn image(path: &Path, rgb: [u8; 3]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(320, 180, image::Rgb(rgb)).save(path).unwrap();
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("walls");
    image(&root.join("Space/earth.png"), [20, 40, 160]);
    image(&root.join("Space/moon.png"), [120, 120, 120]);
    image(&root.join("City/neon.png"), [220, 40, 180]);
    let applied = dir.path().join("applied.txt");
    let paths = Paths::under(dir.path());
    std::fs::create_dir_all(paths.config_file.parent().unwrap()).unwrap();
    std::fs::write(
        &paths.config_file,
        format!(
            "backend = \"command\"\ncommand = \"printf '%s\\\\n' {{path}} >> '{}'\"\n[library]\nroots = [\"{}\"]\nhyde_themes = false\n",
            applied.display(),
            root.display()
        ),
    )
    .unwrap();
    let (tx, events) = async_channel::unbounded();
    let engine = Engine::new(paths, tx);
    Fixture { _dir: dir, root, applied, engine, events }
}

impl Fixture {
    /// Feed worker results back until `done` says to stop.
    fn pump_until(&mut self, mut done: impl FnMut(&[Change], &Engine) -> bool) -> Vec<Change> {
        let mut all = Vec::new();
        loop {
            let event = self.events.recv_blocking().unwrap();
            let changes = self.engine.handle(event);
            all.extend(changes.iter().cloned());
            if done(&changes, &self.engine) {
                return all;
            }
        }
    }

    fn applied(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(&self.applied)
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .collect()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel).canonicalize().unwrap()
    }
}

#[test]
fn thumbnails_and_colours_arrive_for_every_file() {
    let mut f = fixture();
    assert_eq!(f.engine.library.wallpapers.len(), 3);
    f.pump_until(|_, e| e.status().indexing.is_none());
    for w in &f.engine.library.wallpapers {
        assert!(f.engine.metas.ready(&f.engine.paths.cache_dir, &w.key), "{}", w.path.display());
    }
    let neon = f.engine.library.find(&f.path("City/neon.png")).unwrap();
    let meta = f.engine.metas.get(&f.engine.library.wallpapers[neon].key).unwrap();
    assert_eq!(meta.hue, hyprgallery_core::colour::Hue::Pink);
}

#[test]
fn applying_reaches_the_backend_and_becomes_current() {
    let mut f = fixture();
    let earth = f.path("Space/earth.png");
    let changes = f.engine.apply(&earth, None);
    assert_eq!(changes, [Change::Applying]);
    assert!(f.engine.status().applying);
    f.pump_until(|c, _| c.contains(&Change::Current));
    assert_eq!(f.applied(), [earth.clone()]);
    assert_eq!(f.engine.state.current.as_deref(), Some(earth.as_path()));
    assert_eq!(f.engine.status().name.as_deref(), Some("Earth"));
}

#[test]
fn rapid_choices_apply_the_first_and_the_last() {
    let mut f = fixture();
    let (earth, moon, neon) =
        (f.path("Space/earth.png"), f.path("Space/moon.png"), f.path("City/neon.png"));
    f.engine.apply(&earth, None);
    f.engine.apply(&moon, None);
    f.engine.apply(&neon, None);
    let neon_done = neon.clone();
    f.pump_until(|c, e| {
        c.contains(&Change::Current) && e.state.current.as_deref() == Some(neon_done.as_path())
    });
    assert_eq!(f.applied(), [earth, neon]);
}

#[test]
fn a_due_rotation_changes_the_wallpaper_and_moves_on() {
    let mut f = fixture();
    let space = ViewId::parse(&f.engine.library, "Space").unwrap();
    let now = hyprgallery_core::now();
    let mut rotation = Rotation::new(space, 600, false, now);
    rotation.next_at = now - 1;
    f.engine.set_rotation(Some(rotation));
    let changes = f.engine.tick(now);
    assert!(changes.contains(&Change::Applying));
    f.pump_until(|c, _| c.contains(&Change::Current));
    assert_eq!(f.applied(), [f.path("Space/earth.png")]);
    assert_eq!(f.engine.state.rotation.as_ref().unwrap().next_at, now - 1 + 600);
    // Not due again until then.
    assert!(f.engine.tick(now + 10).is_empty());
}

#[test]
fn next_walks_the_album_of_the_current_wallpaper() {
    let mut f = fixture();
    f.engine.apply(&f.path("Space/earth.png"), None);
    f.pump_until(|c, _| c.contains(&Change::Current));
    f.engine.step(None, 1).unwrap();
    f.pump_until(|c, _| c.contains(&Change::Current));
    f.engine.step(None, 1).unwrap();
    f.pump_until(|c, _| c.contains(&Change::Current));
    assert_eq!(
        f.applied(),
        [f.path("Space/earth.png"), f.path("Space/moon.png"), f.path("Space/earth.png")]
    );
}

#[test]
fn moving_the_current_wallpaper_keeps_it_current_and_favourite() {
    let mut f = fixture();
    let moon = f.path("Space/moon.png");
    f.engine.apply(&moon, None);
    f.pump_until(|c, _| c.contains(&Change::Current));
    f.engine.toggle_favourite(&moon);
    let city = f.engine.library.album_by_name("City").unwrap();
    f.engine.move_to(&moon, city).unwrap();
    let moved = f.path("City/moon.png");
    assert!(!moon.exists() && moved.exists());
    assert_eq!(f.engine.state.current.as_deref(), Some(moved.as_path()));
    assert!(f.engine.state.is_favourite(&moved));
    assert_eq!(f.engine.library.count(city), 2);
}

#[test]
fn importing_copies_into_the_album_and_rescans() {
    let mut f = fixture();
    let outside = f._dir.path().join("Downloads/aurora.png");
    image(&outside, [40, 200, 120]);
    let space = f.engine.library.album_by_name("Space").unwrap();
    f.engine.import(vec![outside.clone(), f._dir.path().join("notes.txt")], Some(space)).unwrap();
    let changes =
        f.pump_until(|c, _| c.iter().any(|c| matches!(c, Change::Notice(_) | Change::Error(_))));
    assert!(changes.contains(&Change::Notice("Added 1 wallpaper".into())), "{changes:?}");
    assert!(outside.exists(), "import copies, it does not move");
    assert!(f.engine.library.find(&f.path("Space/aurora.png")).is_some());
}

#[test]
fn a_failing_backend_is_reported_not_fatal() {
    let mut f = fixture();
    let config = f.engine.paths.config_file.clone();
    let text = format!(
        "backend = \"command\"\ncommand = \"exit 7 # {{path}}\"\n[library]\nroots = [\"{}\"]\nhyde_themes = false\n",
        f.root.display()
    );
    std::fs::write(&config, text).unwrap();
    assert!(f.engine.reload_config().contains(&Change::Config));
    f.engine.apply(&f.path("Space/earth.png"), None);
    let changes = f.pump_until(|c, _| c.iter().any(|c| matches!(c, Change::Error(_))));
    assert!(changes.iter().any(|c| matches!(c, Change::Error(_))));
    assert!(f.engine.status().error.is_some());
    assert!(f.engine.state.current.is_none());
}
