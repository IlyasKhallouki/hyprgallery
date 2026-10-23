//! Fuzzy search across every album.

use crate::library::Library;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Best matches first. Matches on the display name, the album, and the file
/// name, so "space earth" finds `Space/earth-shadow.jpg`.
pub fn search(lib: &Library, query: &str, limit: usize) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = lib
        .wallpapers
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let file = w.path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
            let hay = format!("{} {} {}", w.name, lib.albums[w.album].name, file);
            pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher).map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Album, Kind, Origin, Wallpaper};
    use std::path::PathBuf;

    #[test]
    fn finds_by_name_and_album() {
        let lib = Library {
            albums: vec![
                Album { dir: "/w/Space".into(), name: "Space".into(), origin: Origin::Folder },
                Album { dir: "/w/City".into(), name: "City".into(), origin: Origin::Folder },
            ],
            wallpapers: ["earth-shadow.jpg", "moon.jpg", "neon-rain.jpg"]
                .iter()
                .enumerate()
                .map(|(i, f)| Wallpaper {
                    path: PathBuf::from(format!("/w/{f}")),
                    name: crate::naming::display_name(f.trim_end_matches(".jpg")),
                    album: usize::from(i == 2),
                    kind: Kind::Still,
                    size: 0,
                    modified: 0,
                    key: String::new(),
                })
                .collect(),
        };
        assert_eq!(search(&lib, "earth", 10), [0]);
        assert_eq!(search(&lib, "space moon", 10), [1]);
        assert_eq!(search(&lib, "city", 10), [2]);
        assert!(search(&lib, "   ", 10).is_empty());
        assert!(search(&lib, "zzzz", 10).is_empty());
    }
}
