//! A scripted tour of the gallery, recorded to video. Makes the demo for the
//! README and release posts; hidden from `--help`.
//!
//! Meant for a headless compositor (see CONTRIBUTING.md), never a real desktop:
//! it opens the gallery and drives it. Each frame is the window rendered
//! offscreen on top of the desktop wallpaper, which is what a compositor shows,
//! and frames go to ffmpeg at a constant 30 fps.
//!
//! Animations run on the real frame clock, so the recording is paced by the
//! wall clock. When a capture takes longer than a frame, the previous frame is
//! repeated to fill the gap, which keeps the video's speed true to life at the
//! cost of a dropped frame here and there.

use crate::daemon::Daemon;
use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use gtk4 as gtk;
use hyprgallery_core::Layout;
use hyprgallery_core::colour::Hue;
use hyprgallery_core::decode;
use hyprgallery_core::library::Kind;
use hyprgallery_ui::Gallery;
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Demo {
    pub out: PathBuf,
    pub desktop: PathBuf,
}

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: f64 = 30.0;

enum Frame {
    New(Vec<u8>),
    Repeat(u64),
    End,
}

struct Recorder {
    started: Cell<Option<Instant>>,
    written: Cell<u64>,
    captured: Cell<u64>,
    frames: mpsc::Sender<Frame>,
    writer: RefCell<Option<std::thread::JoinHandle<Result<(), String>>>>,
    desktop: RefCell<gdk::Texture>,
    desktop_path: RefCell<PathBuf>,
    desktop_loading: Cell<bool>,
    renderer: RefCell<Option<gsk::Renderer>>,
}

pub fn run(daemon: &Rc<Daemon>, demo: Demo) {
    let desktop = match desktop_texture(&demo.desktop) {
        Some(t) => t,
        None => {
            eprintln!("hyprgallery: cannot read {}", demo.desktop.display());
            daemon.app.quit();
            return;
        }
    };
    let (frames, writer) = match start_ffmpeg(&demo.out) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("hyprgallery: {e}");
            daemon.app.quit();
            return;
        }
    };
    let recorder = Rc::new(Recorder {
        started: Cell::new(None),
        written: Cell::new(0),
        captured: Cell::new(0),
        frames,
        writer: RefCell::new(Some(writer)),
        desktop: RefCell::new(desktop),
        desktop_path: RefCell::new(demo.desktop.clone()),
        desktop_loading: Cell::new(false),
        renderer: RefCell::new(None),
    });

    // Pre-roll, off camera: open once so the renderer, the shaders and the
    // first album's thumbnails are all warm, then close and start recording.
    let gallery = Rc::clone(daemon.gallery());
    gallery.window.fullscreen();
    gallery.show(Some(Layout::Overlay));
    let daemon = Rc::clone(daemon);
    let opened = Instant::now();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        let indexing = daemon.engine.borrow().status().indexing.is_some();
        let ready = !indexing && !gallery.thumbnails_pending();
        if !(ready && opened.elapsed() > Duration::from_millis(2500)) {
            return glib::ControlFlow::Continue;
        }
        recorder.renderer.replace(gallery.window.native().and_then(|n| n.renderer()));
        gallery.hide();
        let (daemon, gallery, recorder) =
            (Rc::clone(&daemon), Rc::clone(&gallery), Rc::clone(&recorder));
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            record(daemon, gallery, recorder);
        });
        glib::ControlFlow::Break
    });
}

fn record(daemon: Rc<Daemon>, gallery: Rc<Gallery>, recorder: Rc<Recorder>) {
    recorder.started.set(Some(Instant::now()));

    let rec = Rc::clone(&recorder);
    let g = Rc::clone(&gallery);
    let d = Rc::clone(&daemon);
    // Below GTK's redraw priority. Capturing renders the whole window, and
    // at default priority a tight capture loop starves layout and painting:
    // the window never gets a size, and every frame comes out empty.
    glib::timeout_add_local_full(Duration::from_millis(4), glib::Priority::LOW, move || {
        rec.tick(&g, &d);
        glib::ControlFlow::Continue
    });

    type Step = Box<dyn FnOnce(&Rc<Gallery>)>;
    let script: Vec<(u64, Step)> = vec![
        (1200, Box::new(|g| g.show(Some(Layout::Overlay)))),
        (2700, Box::new(|g| g.move_selection(1))),
        (3500, Box::new(|g| g.move_selection(1))),
        (4300, Box::new(|g| g.move_selection(1))),
        (5100, Box::new(|g| g.move_selection(4))),
        (
            6200,
            Box::new(|g| {
                g.set_view_named("Forests");
            }),
        ),
        (7600, Box::new(|g| g.move_selection(2))),
        (
            8700,
            Box::new(|g| {
                g.set_view_named("Space");
            }),
        ),
        (10_000, Box::new(|g| g.move_selection(1))),
        (
            11_000,
            Box::new(|g| {
                g.set_view_named("Dusk");
            }),
        ),
        (12_300, Box::new(|g| g.move_selection(3))),
        (
            13_600,
            Box::new(|g| {
                g.set_view_named("By colour");
            }),
        ),
        (15_100, Box::new(|g| g.filter_hue(Some(Hue::Blue)))),
        (16_700, Box::new(|g| g.filter_hue(Some(Hue::Pink)))),
        (
            18_300,
            Box::new(|g| {
                let every = (190.0 * hyprgallery_ui::time_scale()) as u64;
                g.type_into_search("sunset", every, |_, _| {});
            }),
        ),
        (20_500, Box::new(|g| g.move_selection(1))),
        (22_000, Box::new(|g| g.start_peek())),
        (23_900, Box::new(|g| g.end_peek())),
        (25_000, Box::new(|g| g.toggle_favourite_selected())),
        (26_400, Box::new(|g| g.apply_selected())),
        (29_300, Box::new(|g| g.hide())),
        (
            30_700,
            Box::new(|g| {
                g.set_view_named("Dusk");
            }),
        ),
        (30_900, Box::new(|g| g.show(Some(Layout::Popover)))),
        (32_200, Box::new(|g| g.move_selection(1))),
        (33_000, Box::new(|g| g.move_selection(1))),
        (
            34_000,
            Box::new(|g| {
                g.set_view_named("Neon");
            }),
        ),
        (35_400, Box::new(|g| g.move_selection(2))),
        (36_700, Box::new(|g| g.hide())),
    ];
    let end = 38_400;
    // Script times are video time; the session itself runs slowed down.
    let real = |ms: u64| Duration::from_millis((ms as f64 * hyprgallery_ui::time_scale()) as u64);
    for (at, step) in script {
        let g = Rc::clone(&gallery);
        glib::timeout_add_local_once(real(at), move || step(&g));
    }
    glib::timeout_add_local_once(real(end), move || {
        recorder.finish();
        daemon.app.quit();
    });
}

impl Recorder {
    fn tick(self: &Rc<Self>, gallery: &Gallery, daemon: &Daemon) {
        self.follow_desktop(daemon);
        let Some(started) = self.started.get() else { return };
        // Video time runs `time_scale` times faster than the clock, so a
        // slowed-down session plays back at true speed.
        let video_time = started.elapsed().as_secs_f64() / hyprgallery_ui::time_scale();
        let due = (video_time * FPS).floor() as u64 + 1;
        let written = self.written.get();
        if due <= written {
            return;
        }
        // Frames this tick was too late for show what was on screen before.
        if due - written > 1 {
            let _ = self.frames.send(Frame::Repeat(due - written - 1));
        }
        match self.capture(gallery) {
            Some(frame) => {
                let _ = self.frames.send(Frame::New(frame));
                self.captured.set(self.captured.get() + 1);
            }
            None => {
                let _ = self.frames.send(Frame::Repeat(1));
            }
        }
        self.written.set(due);
    }

    /// The desktop wallpaper, then the gallery on top: the compositor's view.
    fn capture(&self, gallery: &Gallery) -> Option<Vec<u8>> {
        let renderer = self.renderer.borrow().clone()?;
        let rect = graphene::Rect::new(0.0, 0.0, W as f32, H as f32);
        let snapshot = gtk::Snapshot::new();
        snapshot.append_texture(&*self.desktop.borrow(), &rect);
        if gallery.window.is_visible() {
            let paintable = gtk::WidgetPaintable::new(Some(&gallery.window));
            paintable.snapshot(&snapshot, W as f64, H as f64);
        }
        let node = snapshot.to_node()?;
        let texture = renderer.render_texture(&node, Some(&rect));
        let mut pixels = vec![0u8; (W * H * 4) as usize];
        texture.download(&mut pixels, (W * 4) as usize);
        Some(pixels)
    }

    /// Show the newly applied wallpaper behind the gallery once applying
    /// finishes, as the desktop would. Decoded off the main thread.
    fn follow_desktop(self: &Rc<Self>, daemon: &Daemon) {
        let current = daemon.engine.borrow().state.current.clone();
        let Some(current) = current else { return };
        if self.desktop_loading.get() || *self.desktop_path.borrow() == current {
            return;
        }
        self.desktop_loading.set(true);
        self.desktop_path.replace(current.clone());
        let (tx, rx) = async_channel::bounded::<Option<decode::Rgb8>>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(desktop_pixels(&current));
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Ok(Some(img)) = rx.recv().await {
                this.desktop.replace(memory_texture(&img));
            }
            this.desktop_loading.set(false);
        });
    }

    fn finish(&self) {
        let _ = self.frames.send(Frame::End);
        if let Some(writer) = self.writer.borrow_mut().take() {
            match writer.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("hyprgallery: ffmpeg: {e}"),
                Err(_) => eprintln!("hyprgallery: the video writer panicked"),
            }
        }
        let (captured, written) = (self.captured.get(), self.written.get());
        println!(
            "{written} frames ({:.1} s), {captured} rendered, {:.0}% unique",
            written as f64 / FPS,
            captured as f64 * 100.0 / written.max(1) as f64
        );
    }
}

fn desktop_pixels(path: &Path) -> Option<decode::Rgb8> {
    let (img, _) = decode::load(path, Kind::Still, W, H).ok()?;
    Some(decode::cover(&img, W, H))
}

fn desktop_texture(path: &Path) -> Option<gdk::Texture> {
    desktop_pixels(path).map(|img| memory_texture(&img))
}

fn memory_texture(img: &decode::Rgb8) -> gdk::Texture {
    let bytes = glib::Bytes::from(&img.data[..]);
    gdk::MemoryTexture::new(
        img.width as i32,
        img.height as i32,
        gdk::MemoryFormat::R8g8b8,
        &bytes,
        img.width as usize * 3,
    )
    .upcast()
}

type Writer = std::thread::JoinHandle<Result<(), String>>;

/// ffmpeg reading raw frames from a pipe, fed by its own thread so a slow
/// encode never stalls the main loop the animations run on.
fn start_ffmpeg(out: &Path) -> Result<(mpsc::Sender<Frame>, Writer), String> {
    let size = format!("{W}x{H}");
    let mut child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "bgra"])
        .args(["-video_size", &size, "-framerate", "30", "-i", "-"])
        .args([
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "12", "-pix_fmt", "yuv420p", "-an",
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting ffmpeg: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("ffmpeg has no stdin")?;
    let (tx, rx) = mpsc::channel::<Frame>();
    let writer = std::thread::spawn(move || {
        let mut last: Option<Vec<u8>> = None;
        for frame in rx {
            match frame {
                Frame::New(pixels) => {
                    stdin.write_all(&pixels).map_err(|e| e.to_string())?;
                    last = Some(pixels);
                }
                Frame::Repeat(n) => {
                    if let Some(pixels) = &last {
                        for _ in 0..n {
                            stdin.write_all(pixels).map_err(|e| e.to_string())?;
                        }
                    }
                }
                Frame::End => break,
            }
        }
        drop(stdin);
        let status = child.wait().map_err(|e| e.to_string())?;
        if status.success() { Ok(()) } else { Err(format!("exited with {status}")) }
    });
    Ok((tx, writer))
}
