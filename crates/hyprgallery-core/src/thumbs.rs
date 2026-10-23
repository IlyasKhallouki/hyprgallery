//! Thumbnails and everything learned while making them.
//!
//! Each wallpaper is decoded once. That one decode produces the thumbnail on
//! disk plus a [`Meta`]: its size, its dominant colour, and a tiny blurred copy
//! the gallery stretches across the screen as its backdrop. After that the
//! original file is not read again until it changes.

use crate::colour::{self, Hue, Rgb};
use crate::decode::{self, Rgb8};
use crate::library::Wallpaper;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Thumbnail size. A little over the widest tile at scale 1, so the hover
/// zoom stays sharp, and small enough that a screenful of decoded tiles is a
/// few tens of megabytes.
pub const THUMB_W: u32 = 512;
pub const THUMB_H: u32 = 288;

/// The backdrop. Small enough to be free to draw at any size, and blurred
/// before it is stored so stretching it shows a wash rather than blocks.
pub const TINY_W: u32 = 48;
pub const TINY_H: u32 = 27;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub width: u32,
    pub height: u32,
    pub colour: Rgb,
    pub hue: Hue,
    pub animated: bool,
    /// `TINY_W` x `TINY_H` RGB, base64.
    pub tiny: String,
}

impl Meta {
    pub fn tiny_rgb(&self) -> Option<Vec<u8>> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.tiny)
            .ok()
            .filter(|v| v.len() == (TINY_W * TINY_H * 3) as usize)
    }

    pub fn accent(&self) -> Rgb {
        colour::accent(self.colour)
    }
}

pub fn thumb_path(cache_dir: &Path, key: &str) -> PathBuf {
    cache_dir.join("thumbs").join(format!("{key}.jpg"))
}

/// Decode `wallpaper` once and produce its thumbnail and [`Meta`].
pub fn generate(wallpaper: &Wallpaper, cache_dir: &Path) -> Result<Meta, String> {
    let (full, decoded) = decode::load(&wallpaper.path, wallpaper.kind, THUMB_W, THUMB_H)
        .map_err(|e| e.to_string())?;
    if full.width == 0 || full.height == 0 {
        return Err("image has no pixels".into());
    }
    let thumb = decode::cover(&full, THUMB_W, THUMB_H);
    drop(full);

    let path = thumb_path(cache_dir, &wallpaper.key);
    write_jpeg(&path, &thumb).map_err(|e| format!("writing thumbnail: {e}"))?;

    let sample = decode::squash(&thumb, 64, 36);
    let samples: Vec<[u8; 3]> = sample.pixels().collect();
    let colour = colour::dominant(&samples);
    Ok(Meta {
        width: decoded.width,
        height: decoded.height,
        colour,
        hue: Hue::of(colour),
        animated: decoded.animated,
        tiny: base64::engine::general_purpose::STANDARD.encode(tiny(&thumb).data),
    })
}

fn tiny(thumb: &Rgb8) -> Rgb8 {
    let small = decode::cover(thumb, TINY_W, TINY_H);
    let Some(buf) = image::RgbImage::from_raw(small.width, small.height, small.data.clone()) else {
        return small;
    };
    let blurred = image::imageops::blur(&buf, 1.2);
    Rgb8 { width: TINY_W, height: TINY_H, data: blurred.into_raw() }
}

fn write_jpeg(path: &Path, img: &Rgb8) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = Vec::with_capacity(64 * 1024);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 88)
        .encode(&img.data, img.width, img.height, image::ExtendedColorType::Rgb8)
        .map_err(std::io::Error::other)?;
    crate::paths::write_atomic(path, &bytes)
}

/// Every [`Meta`] by cache key, persisted as one file.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MetaStore {
    #[serde(default)]
    entries: HashMap<String, Meta>,
    #[serde(skip)]
    dirty: bool,
}

impl MetaStore {
    fn file(cache_dir: &Path) -> PathBuf {
        cache_dir.join("meta.json")
    }

    pub fn load(cache_dir: &Path) -> Self {
        let Ok(bytes) = std::fs::read(Self::file(cache_dir)) else { return Self::default() };
        // A cache is only ever a cache. If it does not parse, start over.
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn get(&self, key: &str) -> Option<&Meta> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: String, meta: Meta) {
        self.entries.insert(key, meta);
        self.dirty = true;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Is the thumbnail for `key` ready to show?
    pub fn ready(&self, cache_dir: &Path, key: &str) -> bool {
        self.entries.contains_key(key) && thumb_path(cache_dir, key).exists()
    }

    /// Forget files that are gone, and delete their thumbnails.
    pub fn prune(&mut self, cache_dir: &Path, live: &HashSet<&str>) {
        let before = self.entries.len();
        self.entries.retain(|k, _| live.contains(k.as_str()));
        if self.entries.len() != before {
            self.dirty = true;
        }
        if let Ok(dir) = std::fs::read_dir(cache_dir.join("thumbs")) {
            for entry in dir.flatten() {
                let name = entry.file_name();
                let key = name.to_string_lossy();
                let key = key.strip_suffix(".jpg").unwrap_or(&key);
                if !live.contains(key) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    pub fn save(&mut self, cache_dir: &Path) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        crate::paths::write_atomic(&Self::file(cache_dir), &bytes)?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Kind, cache_key};

    fn wallpaper(path: PathBuf) -> Wallpaper {
        Wallpaper {
            key: cache_key(&path, 1, 1),
            path,
            name: "x".into(),
            album: 0,
            kind: Kind::Still,
            size: 1,
            modified: 1,
        }
    }

    #[test]
    fn generates_thumbnail_and_meta() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("sky.png");
        image::RgbImage::from_pixel(1920, 1080, image::Rgb([40, 90, 220])).save(&src).unwrap();
        let w = wallpaper(src);
        let cache = dir.path().join("cache");
        let meta = generate(&w, &cache).unwrap();
        assert_eq!((meta.width, meta.height), (1920, 1080));
        assert_eq!(meta.hue, Hue::Blue);
        assert!(meta.tiny_rgb().is_some());
        let thumb = image::open(thumb_path(&cache, &w.key)).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (THUMB_W, THUMB_H));
    }

    #[test]
    fn unreadable_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("broken.jpg");
        std::fs::write(&src, b"\xFF\xD8\xFFnot really").unwrap();
        assert!(generate(&wallpaper(src), dir.path()).is_err());
    }

    #[test]
    fn store_round_trips_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let meta = Meta {
            width: 1,
            height: 1,
            colour: Rgb(1, 2, 3),
            hue: Hue::Mono,
            animated: false,
            tiny: String::new(),
        };
        let mut store = MetaStore::default();
        store.insert("a".into(), meta.clone());
        store.insert("b".into(), meta.clone());
        std::fs::create_dir_all(dir.path().join("thumbs")).unwrap();
        std::fs::write(thumb_path(dir.path(), "b"), b"x").unwrap();
        store.prune(dir.path(), &HashSet::from(["a"]));
        store.save(dir.path()).unwrap();
        let loaded = MetaStore::load(dir.path());
        assert_eq!(loaded.get("a"), Some(&meta));
        assert!(loaded.get("b").is_none());
        assert!(!thumb_path(dir.path(), "b").exists());
    }
}
