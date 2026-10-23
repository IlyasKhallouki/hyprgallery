//! Finding wallpapers and grouping them into albums.
//!
//! An album is a folder. Scanning reads directory entries and file metadata
//! and nothing else, so it stays fast enough to redo on every change; decoding
//! happens later, once per file, in [`crate::thumbs`].

use crate::naming::{display_name, natural_cmp};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// How deep below a root to look. Deep enough for `Anime/Dark/Rain`, shallow
/// enough that pointing it at `~` by mistake does not walk the whole disk.
const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Still,
    /// A GIF. Whether it really has more than one frame is only known after
    /// decoding, see [`crate::thumbs::Meta::animated`].
    Animated,
    Video,
}

impl Kind {
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "webp" | "bmp" | "tif" | "tiff" => Some(Self::Still),
            "gif" => Some(Self::Animated),
            "mp4" | "webm" | "mkv" | "mov" => Some(Self::Video),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// A subfolder of a root.
    Folder,
    /// Files directly inside a root.
    Root,
    /// A HyDE theme's `wallpapers` folder.
    Hyde,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    /// The folder, which is also the album's identity.
    pub dir: PathBuf,
    pub name: String,
    pub origin: Origin,
}

impl Album {
    pub fn id(&self) -> String {
        self.dir.display().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wallpaper {
    pub path: PathBuf,
    pub name: String,
    /// Index into [`Library::albums`].
    pub album: usize,
    pub kind: Kind,
    pub size: u64,
    /// Modification time, seconds since the epoch.
    pub modified: i64,
    /// Names the cached thumbnail. Changes when the file does.
    pub key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    pub albums: Vec<Album>,
    pub wallpapers: Vec<Wallpaper>,
}

/// What to scan.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub roots: Vec<PathBuf>,
    /// `~/.config/hyde/themes`, when HyDE themes should become albums.
    pub hyde_themes: Option<PathBuf>,
}

impl Library {
    pub fn scan(sources: &Sources) -> Self {
        let mut seen_dirs = HashSet::new();
        let mut groups: Vec<(PathBuf, String, Origin, Vec<PathBuf>)> = Vec::new();

        for root in &sources.roots {
            let Ok(root) = root.canonicalize() else { continue };
            if !seen_dirs.insert(root.clone()) {
                continue;
            }
            let mut found = BTreeMap::new();
            walk(&root, 0, &mut found);
            let root_name = file_name(&root);
            for (dir, files) in found {
                let (name, origin) = if dir == root {
                    (root_name.clone(), Origin::Root)
                } else {
                    let rel = dir.strip_prefix(&root).unwrap_or(&dir);
                    let name = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(" / ");
                    (name, Origin::Folder)
                };
                seen_dirs.insert(dir.clone());
                groups.push((dir, name, origin, files));
            }
        }

        if let Some(themes) = &sources.hyde_themes {
            for theme in read_dirs(themes) {
                let dir = theme.join("wallpapers");
                let Ok(dir) = dir.canonicalize() else { continue };
                // A theme folder that is also inside a root would otherwise
                // show up twice.
                if seen_dirs.contains(&dir) || sources.roots.iter().any(|r| dir.starts_with(r)) {
                    continue;
                }
                let files = images_in(&dir);
                if !files.is_empty() {
                    seen_dirs.insert(dir.clone());
                    groups.push((dir, file_name(&theme), Origin::Hyde, files));
                }
            }
        }

        // Your folders first, alphabetically; theme albums after them.
        groups.sort_by(|a, b| {
            (a.2 == Origin::Hyde).cmp(&(b.2 == Origin::Hyde)).then_with(|| natural_cmp(&a.1, &b.1))
        });
        disambiguate(&mut groups);

        let mut library = Library::default();
        for (dir, name, origin, mut files) in groups {
            files.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
            let album = library.albums.len();
            library.albums.push(Album { dir, name, origin });
            for path in files {
                if let Some(wallpaper) = describe(&path, album) {
                    library.wallpapers.push(wallpaper);
                }
            }
        }
        library
    }

    pub fn find(&self, path: &Path) -> Option<usize> {
        self.wallpapers.iter().position(|w| w.path == path)
    }

    pub fn album_by_id(&self, id: &str) -> Option<usize> {
        self.albums.iter().position(|a| a.id() == id)
    }

    /// Match an album the way a person would type it on the command line.
    pub fn album_by_name(&self, name: &str) -> Option<usize> {
        self.albums
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(name))
            .or_else(|| self.album_by_id(name))
            .or_else(|| {
                let lower = name.to_lowercase();
                self.albums.iter().position(|a| a.name.to_lowercase().contains(&lower))
            })
    }

    pub fn in_album(&self, album: usize) -> impl Iterator<Item = usize> + '_ {
        self.wallpapers.iter().enumerate().filter(move |(_, w)| w.album == album).map(|(i, _)| i)
    }

    pub fn count(&self, album: usize) -> usize {
        self.in_album(album).count()
    }

    /// Albums that are plain folders you own, which are the only ones it is
    /// reasonable to move files into.
    pub fn writable_albums(&self) -> impl Iterator<Item = (usize, &Album)> {
        self.albums.iter().enumerate().filter(|(_, a)| a.origin != Origin::Hyde)
    }
}

fn walk(dir: &Path, depth: usize, found: &mut BTreeMap<PathBuf, Vec<PathBuf>>) {
    let files = images_in(dir);
    if !files.is_empty() {
        found.insert(dir.to_owned(), files);
    }
    if depth < MAX_DEPTH {
        for sub in read_dirs(dir) {
            walk(&sub, depth + 1, found);
        }
    }
}

/// Visible subdirectories. Symlinked directories are not followed, which is
/// what keeps a link back to an ancestor from looping.
fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !is_hidden(&e.file_name()))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

fn images_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| !is_hidden(&e.file_name()))
        .map(|e| e.path())
        .filter(|p| Kind::of(p).is_some() && p.is_file())
        .collect()
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

fn describe(path: &Path, album: usize) -> Option<Wallpaper> {
    let kind = Kind::of(path)?;
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Some(Wallpaper {
        path: path.to_owned(),
        name: display_name(&stem),
        album,
        kind,
        size: meta.len(),
        modified,
        key: cache_key(path, meta.len(), modified),
    })
}

/// A key that changes when the file does, without reading the file.
///
/// Hashing contents would be exact, but an 11MB PNG takes longer to hash than a
/// whole folder takes to list. Path, size and mtime catch every real edit.
pub fn cache_key(path: &Path, size: u64, modified: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.update(&size.to_le_bytes());
    hasher.update(&modified.to_le_bytes());
    hasher.finalize().to_hex()[..32].to_owned()
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Two roots can both have a `Space` folder. Tell them apart by where they
/// come from rather than showing two identical names.
fn disambiguate(groups: &mut [(PathBuf, String, Origin, Vec<PathBuf>)]) {
    let names: Vec<String> = groups.iter().map(|g| g.1.clone()).collect();
    for group in groups.iter_mut() {
        if names.iter().filter(|n| **n == group.1).count() > 1 {
            let parent = group.0.parent().map(file_name).unwrap_or_default();
            group.1 = format!("{} ({parent})", group.1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn folders_become_albums() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Wallpapers");
        touch(&root.join("loose.jpg"));
        touch(&root.join("Space/earth.jpg"));
        touch(&root.join("Space/moon10.png"));
        touch(&root.join("Space/moon2.png"));
        touch(&root.join("Anime/Dark/rain.webp"));
        touch(&root.join("Space/notes.txt"));
        touch(&root.join(".hidden/secret.jpg"));
        touch(&root.join("Space/.thumb.jpg"));
        fs::create_dir_all(root.join("Empty")).unwrap();

        let lib = Library::scan(&Sources { roots: vec![root.clone()], hyde_themes: None });
        let names: Vec<_> = lib.albums.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Anime / Dark", "Space", "Wallpapers"]);
        assert_eq!(lib.albums[2].origin, Origin::Root);

        let space = lib.album_by_name("space").unwrap();
        let files: Vec<_> = lib.in_album(space).map(|i| lib.wallpapers[i].name.clone()).collect();
        assert_eq!(files, ["Earth", "Moon2", "Moon10"]);
        assert_eq!(lib.wallpapers.len(), 5);
    }

    #[test]
    fn hyde_themes_come_after_your_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("walls");
        let themes = tmp.path().join("themes");
        touch(&root.join("Zebra/a.jpg"));
        touch(&themes.join("Rosé Pine/wallpapers/b.jpg"));
        touch(&themes.join("Empty Theme/kitty.theme"));

        let lib = Library::scan(&Sources { roots: vec![root], hyde_themes: Some(themes) });
        let names: Vec<_> = lib.albums.iter().map(|a| (a.name.as_str(), a.origin)).collect();
        assert_eq!(names, [("Zebra", Origin::Folder), ("Rosé Pine", Origin::Hyde)]);
    }

    #[test]
    fn same_folder_name_in_two_roots_is_told_apart() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join("a/Space/x.jpg"));
        touch(&tmp.path().join("b/Space/y.jpg"));
        let lib = Library::scan(&Sources {
            roots: vec![tmp.path().join("a"), tmp.path().join("b")],
            hyde_themes: None,
        });
        let names: Vec<_> = lib.albums.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Space (a)", "Space (b)"]);
    }

    #[test]
    fn missing_root_is_empty_not_an_error() {
        let lib = Library::scan(&Sources {
            roots: vec![PathBuf::from("/definitely/not/here")],
            hyde_themes: None,
        });
        assert!(lib.albums.is_empty());
    }

    #[test]
    fn cache_key_follows_the_file() {
        let p = Path::new("/w/a.jpg");
        assert_eq!(cache_key(p, 10, 5), cache_key(p, 10, 5));
        assert_ne!(cache_key(p, 10, 5), cache_key(p, 10, 6));
        assert_ne!(cache_key(p, 10, 5), cache_key(Path::new("/w/b.jpg"), 10, 5));
    }
}
