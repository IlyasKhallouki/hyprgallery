//! Turning files into GPU textures off the main thread.
//!
//! Three kinds, each with its own small cache: thumbnails for tiles, the
//! blurred backdrop, and full-screen peek images. Decoding happens on worker
//! threads; the main thread only ever receives finished textures, so scrolling
//! through a folder that has never been opened does not drop frames.

use gtk::prelude::Cast;
use gtk::{gdk, glib};
use gtk4 as gtk;
use hyprgallery_core::decode::{self, Rgb8};
use hyprgallery_core::library::Kind;
use hyprgallery_core::thumbs::Meta;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// About a hundred megabytes of decoded thumbnails, several screens' worth.
const THUMBS: usize = 180;
const BACKDROPS: usize = 48;
/// Peek images are screen-sized. Keep the few around the focus.
const PEEKS: usize = 3;

enum Job {
    Thumb { id: String, path: PathBuf },
    Backdrop { id: String, meta: Box<Meta> },
    Peek { id: String, path: PathBuf, kind: Kind, size: (u32, u32), generation: u64 },
}

struct Done {
    id: String,
    texture: Option<gdk::Texture>,
    /// A peek job that was skipped because focus had moved on. Its waiters
    /// belong to a newer job for the same image, so this result is not theirs.
    skipped: bool,
}

type Waiter = Box<dyn FnOnce(Option<gdk::Texture>)>;

struct Lru {
    cap: usize,
    clock: u64,
    map: HashMap<String, (gdk::Texture, u64)>,
}

impl Lru {
    fn new(cap: usize) -> Self {
        Self { cap, clock: 0, map: HashMap::new() }
    }

    fn get(&mut self, id: &str) -> Option<gdk::Texture> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(id).map(|(t, used)| {
            *used = clock;
            t.clone()
        })
    }

    fn put(&mut self, id: String, texture: gdk::Texture) {
        self.clock += 1;
        self.map.insert(id, (texture, self.clock));
        while self.map.len() > self.cap {
            let Some(oldest) =
                self.map.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone())
            else {
                break;
            };
            self.map.remove(&oldest);
        }
    }
}

pub struct Loader {
    caches: RefCell<[Lru; 3]>,
    waiting: RefCell<HashMap<String, Vec<Waiter>>>,
    fast: mpsc::Sender<Job>,
    slow: mpsc::Sender<Job>,
    peek_generation: Arc<AtomicU64>,
}

fn cache_of(id: &str) -> usize {
    match id.as_bytes().first() {
        Some(b't') => 0,
        Some(b'b') => 1,
        _ => 2,
    }
}

impl Loader {
    pub fn new() -> Rc<Self> {
        let (done_tx, done_rx) = async_channel::unbounded::<Done>();
        let (fast, fast_rx) = mpsc::channel::<Job>();
        let (slow, slow_rx) = mpsc::channel::<Job>();
        let peek_generation = Arc::new(AtomicU64::new(0));

        // Two for thumbnails, which are small and arrive in bursts when a grid
        // fills, and one for peek images, which are large and only ever wanted
        // one at a time. Separate queues so a slow 8K decode never holds up the
        // thumbnails behind it.
        let fast_rx = Arc::new(Mutex::new(fast_rx));
        for n in 0..2 {
            spawn_worker(
                format!("hg-tex-{n}"),
                Arc::clone(&fast_rx),
                done_tx.clone(),
                Arc::clone(&peek_generation),
            );
        }
        spawn_worker(
            "hg-peek".into(),
            Arc::new(Mutex::new(slow_rx)),
            done_tx,
            Arc::clone(&peek_generation),
        );

        let loader = Rc::new(Self {
            caches: RefCell::new([Lru::new(THUMBS), Lru::new(BACKDROPS), Lru::new(PEEKS)]),
            waiting: RefCell::new(HashMap::new()),
            fast,
            slow,
            peek_generation,
        });
        let weak = Rc::downgrade(&loader);
        glib::spawn_future_local(async move {
            while let Ok(done) = done_rx.recv().await {
                let Some(loader) = weak.upgrade() else { break };
                loader.finish(done);
            }
        });
        loader
    }

    fn finish(&self, done: Done) {
        if done.skipped {
            return;
        }
        if let Some(texture) = &done.texture {
            self.caches.borrow_mut()[cache_of(&done.id)].put(done.id.clone(), texture.clone());
        }
        let waiters = self.waiting.borrow_mut().remove(&done.id).unwrap_or_default();
        for waiter in waiters {
            waiter(done.texture.clone());
        }
    }

    fn cached(&self, id: &str) -> Option<gdk::Texture> {
        self.caches.borrow_mut()[cache_of(id)].get(id)
    }

    /// Calls `then` with the texture, synchronously if it is cached.
    fn request(
        &self,
        id: String,
        job: impl FnOnce() -> Job,
        slow: bool,
        then: impl FnOnce(Option<gdk::Texture>) + 'static,
    ) {
        if let Some(texture) = self.cached(&id) {
            then(Some(texture));
            return;
        }
        let mut waiting = self.waiting.borrow_mut();
        let first = !waiting.contains_key(&id);
        waiting.entry(id).or_default().push(Box::new(then));
        drop(waiting);
        if first {
            let queue = if slow { &self.slow } else { &self.fast };
            let _ = queue.send(job());
        }
    }

    pub fn cached_thumb(&self, key: &str) -> Option<gdk::Texture> {
        self.cached(&format!("t{key}"))
    }

    pub fn thumb(
        &self,
        key: &str,
        path: PathBuf,
        then: impl FnOnce(Option<gdk::Texture>) + 'static,
    ) {
        let id = format!("t{key}");
        self.request(id.clone(), || Job::Thumb { id, path }, false, then);
    }

    pub fn backdrop(
        &self,
        key: &str,
        meta: &Meta,
        then: impl FnOnce(Option<gdk::Texture>) + 'static,
    ) {
        let id = format!("b{key}");
        self.request(
            id.clone(),
            || Job::Backdrop { id, meta: Box::new(meta.clone()) },
            false,
            then,
        );
    }

    /// A screen-sized image. Asking for a new one abandons any older request
    /// still waiting, since focus has moved on.
    pub fn peek(
        &self,
        key: &str,
        path: PathBuf,
        kind: Kind,
        size: (u32, u32),
        then: impl FnOnce(Option<gdk::Texture>) + 'static,
    ) {
        let id = format!("p{key}:{}x{}", size.0, size.1);
        if let Some(texture) = self.cached(&id) {
            then(Some(texture));
            return;
        }
        // Already on its way: wait for that job. Starting a new generation
        // here would make the job in flight skip itself, with nobody left to
        // answer.
        if let Some(waiters) = self.waiting.borrow_mut().get_mut(&id) {
            waiters.push(Box::new(then));
            return;
        }
        let generation = self.peek_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.waiting.borrow_mut().retain(|k, _| !k.starts_with('p'));
        self.request(id.clone(), || Job::Peek { id, path, kind, size, generation }, true, then);
    }
}

fn spawn_worker(
    name: String,
    jobs: Arc<Mutex<mpsc::Receiver<Job>>>,
    done: async_channel::Sender<Done>,
    peek_generation: Arc<AtomicU64>,
) {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            loop {
                let job = {
                    let Ok(rx) = jobs.lock() else { return };
                    match rx.recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    }
                };
                let (id, texture, skipped) = match job {
                    Job::Thumb { id, path } => {
                        let texture = std::fs::read(&path).ok().and_then(|bytes| {
                            gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()
                        });
                        (id, texture, false)
                    }
                    Job::Backdrop { id, meta } => {
                        (id, meta.backdrop().map(|img| memory_texture(&img)), false)
                    }
                    Job::Peek { id, path, kind, size, generation } => {
                        // Focus moved on while this waited. Skip the decode.
                        if peek_generation.load(Ordering::SeqCst) != generation {
                            (id, None, true)
                        } else {
                            let texture =
                                decode::load(&path, kind, size.0, size.1).ok().map(|(img, _)| {
                                    memory_texture(&decode::cover(&img, size.0, size.1))
                                });
                            (id, texture, false)
                        }
                    }
                };
                if done.send_blocking(Done { id, texture, skipped }).is_err() {
                    return;
                }
            }
        })
        .expect("spawn texture worker");
}

pub fn memory_texture(img: &Rgb8) -> gdk::Texture {
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
