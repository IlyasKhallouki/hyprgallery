//! What the gallery can show: a folder album, or one of the smart albums that
//! are computed from the library rather than stored anywhere.

use crate::colour::Hue;
use crate::library::{Kind, Library};
use crate::naming::natural_cmp;
use crate::state::State;
use crate::thumbs::MetaStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum ViewId {
    /// A folder, by its path.
    Album(String),
    Favourites,
    Recent,
    Colour,
    Animated,
}

/// One entry in the gallery's album row.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewInfo {
    pub id: ViewId,
    pub label: String,
    pub count: usize,
    pub smart: bool,
}

impl ViewId {
    /// Parse what someone typed after `--album`.
    pub fn parse(lib: &Library, text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "favourites" | "favorites" | "favs" => Some(Self::Favourites),
            "recent" => Some(Self::Recent),
            "colour" | "color" | "by colour" | "by color" => Some(Self::Colour),
            "animated" => Some(Self::Animated),
            _ => lib.album_by_name(text).map(|i| Self::Album(lib.albums[i].id())),
        }
    }

    pub fn label(&self, lib: &Library) -> String {
        match self {
            Self::Album(id) => lib
                .album_by_id(id)
                .map(|i| lib.albums[i].name.clone())
                .unwrap_or_else(|| "Missing album".into()),
            Self::Favourites => "Favourites".into(),
            Self::Recent => "Recent".into(),
            Self::Colour => "By colour".into(),
            Self::Animated => "Animated".into(),
        }
    }
}

/// Is `i` animated, as far as anyone knows yet?
pub fn is_animated(lib: &Library, metas: &MetaStore, i: usize) -> bool {
    let w = &lib.wallpapers[i];
    match w.kind {
        Kind::Video => true,
        Kind::Animated => metas.get(&w.key).is_none_or(|m| m.animated),
        Kind::Still => false,
    }
}

/// The wallpapers in a view, in display order, as indices into the library.
pub fn resolve(
    lib: &Library,
    state: &State,
    metas: &MetaStore,
    view: &ViewId,
    recent: usize,
) -> Vec<usize> {
    match view {
        ViewId::Album(id) => match lib.album_by_id(id) {
            Some(album) => lib.in_album(album).collect(),
            None => Vec::new(),
        },
        ViewId::Favourites => state.favourites.iter().filter_map(|p| lib.find(p)).collect(),
        ViewId::Recent => {
            let mut all: Vec<usize> = (0..lib.wallpapers.len()).collect();
            all.sort_by(|&a, &b| {
                lib.wallpapers[b]
                    .modified
                    .cmp(&lib.wallpapers[a].modified)
                    .then_with(|| natural_cmp(&lib.wallpapers[a].name, &lib.wallpapers[b].name))
            });
            all.truncate(recent);
            all
        }
        ViewId::Colour => {
            // Rainbow order: hue bucket, then hue within it, then light to dark.
            // Files not analysed yet go last rather than being left out.
            let mut all: Vec<usize> = (0..lib.wallpapers.len()).collect();
            let key = |i: usize| match metas.get(&lib.wallpapers[i].key) {
                Some(m) => {
                    let (h, _, l) = m.colour.hsl();
                    let h = if m.hue == Hue::Red && h > 180.0 { h - 360.0 } else { h };
                    (m.hue as u8, (h * 10.0) as i32, (-(l * 1000.0)) as i32)
                }
                None => (u8::MAX, 0, 0),
            };
            all.sort_by_key(|&i| key(i));
            all
        }
        ViewId::Animated => {
            (0..lib.wallpapers.len()).filter(|&i| is_animated(lib, metas, i)).collect()
        }
    }
}

/// Every view worth offering, in the order the album row shows them: the
/// smart albums first, since they span the whole library and stay put while
/// folders come and go, then your folders.
pub fn available(lib: &Library, state: &State, metas: &MetaStore, recent: usize) -> Vec<ViewInfo> {
    let mut views = Vec::new();
    if !lib.wallpapers.is_empty() {
        let favourites = state.favourites.iter().filter(|p| lib.find(p).is_some()).count();
        // Favourites is always offered, empty or not, because an empty album
        // with a hint is how anyone learns favouriting exists.
        views.push(smart(ViewId::Favourites, "Favourites", favourites));
        views.push(smart(ViewId::Recent, "Recent", lib.wallpapers.len().min(recent)));
        views.push(smart(ViewId::Colour, "By colour", lib.wallpapers.len()));
        let animated = (0..lib.wallpapers.len()).filter(|&i| is_animated(lib, metas, i)).count();
        if animated > 0 {
            views.push(smart(ViewId::Animated, "Animated", animated));
        }
    }
    views.extend(lib.albums.iter().enumerate().map(|(i, a)| ViewInfo {
        id: ViewId::Album(a.id()),
        label: a.name.clone(),
        count: lib.count(i),
        smart: false,
    }));
    views
}

fn smart(id: ViewId, label: &str, count: usize) -> ViewInfo {
    ViewInfo { id, label: label.into(), count, smart: true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Album, Origin, Wallpaper};
    use std::path::PathBuf;

    fn lib() -> Library {
        let album = |n: &str| Album {
            dir: PathBuf::from(format!("/w/{n}")),
            name: n.into(),
            origin: Origin::Folder,
        };
        let wall = |n: &str, album: usize, kind: Kind, modified: i64| Wallpaper {
            path: PathBuf::from(format!("/w/{n}")),
            name: n.into(),
            album,
            kind,
            size: 1,
            modified,
            key: n.into(),
        };
        Library {
            albums: vec![album("Space"), album("City")],
            wallpapers: vec![
                wall("earth", 0, Kind::Still, 100),
                wall("moon", 0, Kind::Still, 300),
                wall("rain", 1, Kind::Animated, 200),
                wall("loop", 1, Kind::Video, 50),
            ],
        }
    }

    #[test]
    fn album_and_recent() {
        let lib = lib();
        let s = State::default();
        let m = MetaStore::default();
        let space = ViewId::Album("/w/Space".into());
        assert_eq!(resolve(&lib, &s, &m, &space, 48), [0, 1]);
        assert_eq!(resolve(&lib, &s, &m, &ViewId::Recent, 2), [1, 2]);
        assert_eq!(resolve(&lib, &s, &m, &ViewId::Animated, 48), [2, 3]);
    }

    #[test]
    fn favourites_keep_their_order_and_skip_missing_files() {
        let lib = lib();
        let mut s = State::default();
        s.toggle_favourite(std::path::Path::new("/w/earth"));
        s.toggle_favourite(std::path::Path::new("/w/gone"));
        s.toggle_favourite(std::path::Path::new("/w/rain"));
        let m = MetaStore::default();
        assert_eq!(resolve(&lib, &s, &m, &ViewId::Favourites, 48), [2, 0]);
        let views = available(&lib, &s, &m, 48);
        let fav = views.iter().find(|v| v.id == ViewId::Favourites).unwrap();
        assert_eq!(fav.count, 2);
        let order: Vec<_> = views.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(order, ["Favourites", "Recent", "By colour", "Animated", "Space", "City"]);
    }

    #[test]
    fn parses_album_names_from_the_command_line() {
        let lib = lib();
        assert_eq!(ViewId::parse(&lib, "space"), Some(ViewId::Album("/w/Space".into())));
        assert_eq!(ViewId::parse(&lib, "Favorites"), Some(ViewId::Favourites));
        assert_eq!(ViewId::parse(&lib, "nope"), None);
    }

    #[test]
    fn view_id_serialises_readably() {
        let json = serde_json::to_string(&ViewId::Album("/w/Space".into())).unwrap();
        assert_eq!(json, r#"{"kind":"album","id":"/w/Space"}"#);
        let json = serde_json::to_string(&ViewId::Recent).unwrap();
        assert_eq!(json, r#"{"kind":"recent"}"#);
    }
}
