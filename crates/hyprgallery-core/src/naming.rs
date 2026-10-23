//! Turning file names into something worth reading.

use std::cmp::Ordering;

/// A display name for a wallpaper file stem.
///
/// Downloaded wallpapers are named for search engines, not people:
/// `earth-shadow-5568x3712-27324` is "Earth shadow". Separators become spaces
/// and resolution and id noise is dropped, unless that would leave almost
/// nothing, in which case the original words are kept.
pub fn display_name(stem: &str) -> String {
    let words: Vec<&str> = stem
        .split(|c: char| c == '-' || c == '_' || c == '.' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect();
    let kept: Vec<&str> = words.iter().copied().filter(|w| !is_noise(w)).collect();
    let chosen = if kept.join(" ").chars().count() >= 4 { kept } else { words };
    let joined = chosen.join(" ");
    capitalise(if joined.is_empty() { stem } else { &joined })
}

fn is_noise(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    let is_resolution = lower.split_once('x').is_some_and(|(w, h)| {
        !w.is_empty()
            && !h.is_empty()
            && w.chars().all(|c| c.is_ascii_digit())
            && h.chars().all(|c| c.is_ascii_digit())
    });
    let is_id = word.len() >= 4 && word.chars().all(|c| c.is_ascii_digit());
    is_resolution
        || is_id
        || matches!(lower.as_str(), "4k" | "5k" | "8k" | "uhd" | "fhd" | "qhd" | "hd" | "wallpaper")
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Compare names the way people count: `wall2` before `wall10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (na, ra) = split_digits(a);
                let (nb, rb) = split_digits(b);
                let (ta, tb) = (trim_zeros(na), trim_zeros(nb));
                let order = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if order != Ordering::Equal {
                    return order;
                }
                (a, b) = (ra, rb);
            }
            (Some(x), Some(y)) => {
                let order = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                (a, b) = (&a[1..], &b[1..]);
            }
        }
    }
}

fn split_digits(s: &[u8]) -> (&[u8], &[u8]) {
    let end = s.iter().position(|c| !c.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_download_noise() {
        assert_eq!(display_name("earth-shadow-5568x3712-27324"), "Earth shadow");
        assert_eq!(display_name("burning_cherry"), "Burning cherry");
        assert_eq!(display_name("mountain_dragon_pink"), "Mountain dragon pink");
        assert_eq!(display_name("city-night-4k-wallpaper"), "City night");
    }

    #[test]
    fn keeps_words_when_noise_is_all_there_is() {
        assert_eq!(display_name("IMG_20240101_123456"), "IMG 20240101 123456");
        assert_eq!(display_name("1920x1080"), "1920x1080");
        assert_eq!(display_name("rosé"), "Rosé");
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["wall10", "wall2", "Wall1", "wall02b", "alpha"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["alpha", "Wall1", "wall2", "wall02b", "wall10"]);
    }
}
