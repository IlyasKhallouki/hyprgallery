//! Dominant colour, hue buckets for the "By colour" album, and the accent the
//! gallery takes from whatever wallpaper is focused.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// Hue in degrees, saturation and lightness in 0..=1.
    pub fn hsl(self) -> (f32, f32, f32) {
        let r = self.0 as f32 / 255.0;
        let g = self.1 as f32 / 255.0;
        let b = self.2 as f32 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        if d < f32::EPSILON {
            return (0.0, 0.0, l);
        }
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            60.0 * (((g - b) / d).rem_euclid(6.0))
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h, s.clamp(0.0, 1.0), l)
    }

    pub fn from_hsl(h: f32, s: f32, l: f32) -> Self {
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let hp = h.rem_euclid(360.0) / 60.0;
        let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match hp as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = l - c / 2.0;
        let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgb(to(r), to(g), to(b))
    }
}

/// The groups the "By colour" album is split into, in rainbow order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hue {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    /// Greys, near-blacks and near-whites, where hue means nothing.
    Mono,
}

impl Hue {
    pub const ALL: [Hue; 9] = [
        Hue::Red,
        Hue::Orange,
        Hue::Yellow,
        Hue::Green,
        Hue::Teal,
        Hue::Blue,
        Hue::Purple,
        Hue::Pink,
        Hue::Mono,
    ];

    pub fn of(c: Rgb) -> Self {
        let (h, s, l) = c.hsl();
        if s < 0.16 || l < 0.07 || l > 0.95 {
            return Hue::Mono;
        }
        match h {
            h if !(15.0..345.0).contains(&h) => Hue::Red,
            h if h < 40.0 => Hue::Orange,
            h if h < 68.0 => Hue::Yellow,
            h if h < 160.0 => Hue::Green,
            h if h < 196.0 => Hue::Teal,
            h if h < 255.0 => Hue::Blue,
            h if h < 290.0 => Hue::Purple,
            _ => Hue::Pink,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Hue::Red => "Red",
            Hue::Orange => "Orange",
            Hue::Yellow => "Yellow",
            Hue::Green => "Green",
            Hue::Teal => "Teal",
            Hue::Blue => "Blue",
            Hue::Purple => "Purple",
            Hue::Pink => "Pink",
            Hue::Mono => "Mono",
        }
    }

    /// A representative colour for the filter swatch.
    pub fn swatch(self) -> Rgb {
        match self {
            Hue::Red => Rgb(0xe0, 0x4f, 0x5f),
            Hue::Orange => Rgb(0xf0, 0x8a, 0x4b),
            Hue::Yellow => Rgb(0xf2, 0xc9, 0x4c),
            Hue::Green => Rgb(0x6c, 0xc0, 0x6a),
            Hue::Teal => Rgb(0x3f, 0xb8, 0xaf),
            Hue::Blue => Rgb(0x4f, 0x8c, 0xe8),
            Hue::Purple => Rgb(0x9b, 0x6c, 0xe0),
            Hue::Pink => Rgb(0xe8, 0x7a, 0xc0),
            Hue::Mono => Rgb(0x9a, 0x9a, 0xa0),
        }
    }
}

/// The colour a person would name the image by.
///
/// Plain averaging turns a purple-and-orange sunset into mud, and plain
/// frequency picks the black sky of every night photo. So: cluster the pixels,
/// then prefer clusters that are both large and saturated. A dark image with a
/// small neon sign still reads as dark, since its sign is too small to win.
pub fn dominant(pixels: &[[u8; 3]]) -> Rgb {
    if pixels.is_empty() {
        return Rgb(0, 0, 0);
    }
    const K: usize = 5;
    let as_f32 = |p: &[u8; 3]| [p[0] as f32, p[1] as f32, p[2] as f32];
    // Farthest-point seeding: start at the median brightness, then keep adding
    // whichever pixel is furthest from every seed so far. Deterministic, and it
    // gives a small vivid region its own cluster instead of averaging it into
    // the background, which evenly spaced seeds would do.
    let mut centres: Vec<[f32; 3]> = {
        let mut sorted: Vec<&[u8; 3]> = pixels.iter().collect();
        sorted.sort_by_key(|p| p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32);
        let mut centres = vec![as_f32(sorted[sorted.len() / 2])];
        while centres.len() < K {
            let far = pixels
                .iter()
                .map(as_f32)
                .max_by(|a, b| {
                    let near = |p: &[f32; 3]| {
                        centres.iter().map(|c| dist(p, c)).fold(f32::INFINITY, f32::min)
                    };
                    near(a).total_cmp(&near(b))
                })
                .unwrap_or([0.0; 3]);
            centres.push(far);
        }
        centres
    };
    let mut counts = [0usize; K];
    for _ in 0..10 {
        let mut sums = [[0f32; 3]; K];
        counts = [0; K];
        for p in pixels {
            let p = [p[0] as f32, p[1] as f32, p[2] as f32];
            let nearest = (0..K)
                .min_by(|&a, &b| dist(&p, &centres[a]).total_cmp(&dist(&p, &centres[b])))
                .unwrap_or(0);
            counts[nearest] += 1;
            for c in 0..3 {
                sums[nearest][c] += p[c];
            }
        }
        for k in 0..K {
            if counts[k] > 0 {
                centres[k] = sums[k].map(|s| s / counts[k] as f32);
            }
        }
    }
    let to_rgb = |c: &[f32; 3]| Rgb(c[0].round() as u8, c[1].round() as u8, c[2].round() as u8);
    (0..K)
        .filter(|&k| counts[k] > 0)
        .max_by(|&a, &b| {
            let score = |k: usize| {
                let (_, s, l) = to_rgb(&centres[k]).hsl();
                // Lightness extremes carry no colour even when "saturated".
                let vivid = s * (1.0 - (2.0 * l - 1.0).abs());
                counts[k] as f32 * (0.2 + vivid)
            };
            score(a).total_cmp(&score(b))
        })
        .map(|k| to_rgb(&centres[k]))
        .unwrap_or(Rgb(0, 0, 0))
}

fn dist(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    // Weighted towards green the way the eye is, which keeps the clusters
    // closer to what looks like one colour.
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    2.0 * d[0] * d[0] + 4.0 * d[1] * d[1] + 3.0 * d[2] * d[2]
}

/// An accent that reads on the gallery's dark glass.
///
/// The dominant colour itself is often too dark or too grey to draw a focus
/// ring with. Keep its hue, give it enough colour and light to stand out.
pub fn accent(c: Rgb) -> Rgb {
    let (h, s, l) = c.hsl();
    if Hue::of(c) == Hue::Mono {
        return Rgb::from_hsl(h, s.min(0.12), 0.86);
    }
    Rgb::from_hsl(h, s.clamp(0.5, 0.9), l.clamp(0.66, 0.78))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_round_trips() {
        for c in [Rgb(255, 0, 0), Rgb(18, 52, 86), Rgb(200, 180, 40), Rgb(128, 128, 128)] {
            let (h, s, l) = c.hsl();
            let back = Rgb::from_hsl(h, s, l);
            for (a, b) in [(c.0, back.0), (c.1, back.1), (c.2, back.2)] {
                assert!((a as i32 - b as i32).abs() <= 1, "{c:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn buckets_by_hue() {
        assert_eq!(Hue::of(Rgb(220, 40, 50)), Hue::Red);
        assert_eq!(Hue::of(Rgb(240, 140, 40)), Hue::Orange);
        assert_eq!(Hue::of(Rgb(40, 90, 220)), Hue::Blue);
        assert_eq!(Hue::of(Rgb(150, 80, 220)), Hue::Purple);
        assert_eq!(Hue::of(Rgb(235, 188, 186)), Hue::Red);
        assert_eq!(Hue::of(Rgb(30, 30, 32)), Hue::Mono);
        assert_eq!(Hue::of(Rgb(3, 3, 30)), Hue::Mono);
    }

    #[test]
    fn dominant_prefers_a_large_colourful_area() {
        let mut pixels = vec![[10, 10, 12]; 400];
        pixels.extend(vec![[30, 80, 200]; 500]);
        pixels.extend(vec![[250, 60, 60]; 20]);
        assert_eq!(Hue::of(dominant(&pixels)), Hue::Blue);
    }

    #[test]
    fn dark_photo_with_a_small_sign_stays_dark() {
        let mut pixels = vec![[12, 12, 14]; 950];
        pixels.extend(vec![[255, 40, 200]; 50]);
        assert_eq!(Hue::of(dominant(&pixels)), Hue::Mono);
    }

    #[test]
    fn accent_is_light_enough() {
        let (_, _, l) = accent(Rgb(20, 10, 60)).hsl();
        assert!(l >= 0.65);
        assert_eq!(Hue::of(accent(Rgb(20, 20, 20))), Hue::Mono);
    }
}
