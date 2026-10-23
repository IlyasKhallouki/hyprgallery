//! Changing the wallpaper on a timer.

use crate::view::ViewId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Shorter than this is a slideshow, not a wallpaper, and on HyDE every change
/// recolours the whole desktop.
pub const MIN_INTERVAL: u64 = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rotation {
    pub view: ViewId,
    pub every_secs: u64,
    pub shuffle: bool,
    /// When the next change is due, seconds since the epoch. Persisted, so a
    /// restart does not reset the clock.
    pub next_at: i64,
}

impl Rotation {
    pub fn new(view: ViewId, every_secs: u64, shuffle: bool, now: i64) -> Self {
        let every_secs = every_secs.max(MIN_INTERVAL);
        Self { view, every_secs, shuffle, next_at: now + every_secs as i64 }
    }

    pub fn due(&self, now: i64) -> bool {
        now >= self.next_at
    }

    /// Move the deadline on after a change.
    ///
    /// Keeps to the original schedule when it can. After a suspend that missed
    /// several changes it does one and starts afresh, rather than firing off
    /// every missed change in a row.
    pub fn advance(&mut self, now: i64) {
        let every = self.every_secs as i64;
        self.next_at = if self.next_at + every > now { self.next_at + every } else { now + every };
    }
}

/// Parse `90s`, `15m`, `1h`, `1h30m`, `2d`, `hourly` or `daily`.
pub fn parse_interval(text: &str) -> Result<u64, String> {
    let text = text.trim().to_ascii_lowercase();
    let named = match text.as_str() {
        "hourly" => Some(3600),
        "daily" => Some(86_400),
        _ => None,
    };
    let secs = match named {
        Some(secs) => secs,
        None => {
            let mut total = 0u64;
            let mut number = String::new();
            for c in text.chars() {
                if c.is_ascii_digit() {
                    number.push(c);
                    continue;
                }
                let unit = match c {
                    's' => 1,
                    'm' => 60,
                    'h' => 3600,
                    'd' => 86_400,
                    ' ' => continue,
                    other => return Err(format!("unknown unit `{other}` in `{text}`")),
                };
                let n: u64 = number.parse().map_err(|_| format!("missing number in `{text}`"))?;
                total += n * unit;
                number.clear();
            }
            if !number.is_empty() {
                return Err(format!("`{text}` needs a unit, like {number}m or {number}h"));
            }
            total
        }
    };
    if secs < MIN_INTERVAL {
        return Err(format!("the shortest interval is {} seconds", MIN_INTERVAL));
    }
    Ok(secs)
}

/// The way the interval is said back: `every 15 min`, `every 1 h 30 min`.
pub fn describe_interval(secs: u64) -> String {
    match secs {
        86_400 => "daily".into(),
        3600 => "hourly".into(),
        s if s % 86_400 == 0 => format!("every {} days", s / 86_400),
        s => {
            let (h, m) = (s / 3600, s % 3600 / 60);
            match (h, m) {
                (0, m) => format!("every {m} min"),
                (h, 0) => format!("every {h} h"),
                (h, m) => format!("every {h} h {m} min"),
            }
        }
    }
}

/// The wallpaper after `current` in `order`, or a random one.
///
/// Shuffling avoids anything shown recently, so a small album cycles through
/// everything before repeating instead of bouncing between two favourites.
pub fn pick(
    order: &[usize],
    paths: &[PathBuf],
    current: Option<usize>,
    shuffle: bool,
    history: &[PathBuf],
    seed: u64,
) -> Option<usize> {
    if order.is_empty() {
        return None;
    }
    if !shuffle {
        let pos = current.and_then(|c| order.iter().position(|&i| i == c));
        return Some(match pos {
            Some(p) => order[(p + 1) % order.len()],
            None => order[0],
        });
    }
    let avoid = (order.len() / 2).min(history.len());
    let recent = &history[..avoid];
    let fresh: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| Some(i) != current && !recent.contains(&paths[i]))
        .collect();
    let pool: Vec<usize> = if fresh.is_empty() {
        order.iter().copied().filter(|&i| Some(i) != current).collect()
    } else {
        fresh
    };
    if pool.is_empty() {
        return Some(order[0]);
    }
    Some(pool[(splitmix(seed) % pool.len() as u64) as usize])
}

/// Step forwards or back through `order` from `current`.
pub fn step(order: &[usize], current: Option<usize>, delta: isize) -> Option<usize> {
    if order.is_empty() {
        return None;
    }
    let len = order.len() as isize;
    let pos = current.and_then(|c| order.iter().position(|&i| i == c));
    Some(match pos {
        Some(p) => order[((p as isize + delta).rem_euclid(len)) as usize],
        None if delta < 0 => order[order.len() - 1],
        None => order[0],
    })
}

/// A seed good enough for picking a wallpaper. Not for anything else.
pub fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ ((std::process::id() as u64) << 32)
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_intervals() {
        assert_eq!(parse_interval("15m"), Ok(900));
        assert_eq!(parse_interval("1h30m"), Ok(5400));
        assert_eq!(parse_interval("2d"), Ok(172_800));
        assert_eq!(parse_interval("daily"), Ok(86_400));
        assert!(parse_interval("10s").is_err());
        assert!(parse_interval("15").unwrap_err().contains("15m"));
        assert!(parse_interval("5x").is_err());
    }

    #[test]
    fn describes_intervals() {
        assert_eq!(describe_interval(900), "every 15 min");
        assert_eq!(describe_interval(3600), "hourly");
        assert_eq!(describe_interval(5400), "every 1 h 30 min");
        assert_eq!(describe_interval(86_400), "daily");
        assert_eq!(describe_interval(7200), "every 2 h");
    }

    #[test]
    fn advance_keeps_schedule_but_skips_missed_changes() {
        let mut r = Rotation::new(ViewId::Recent, 600, false, 1000);
        assert_eq!(r.next_at, 1600);
        assert!(!r.due(1599) && r.due(1600));
        r.advance(1605);
        assert_eq!(r.next_at, 2200);
        r.advance(100_000);
        assert_eq!(r.next_at, 100_600);
    }

    #[test]
    fn sequential_wraps_around() {
        let order = [4, 7, 9];
        let paths: Vec<PathBuf> = (0..10).map(|i| PathBuf::from(format!("/{i}"))).collect();
        assert_eq!(pick(&order, &paths, Some(9), false, &[], 0), Some(4));
        assert_eq!(pick(&order, &paths, None, false, &[], 0), Some(4));
        assert_eq!(step(&order, Some(4), -1), Some(9));
        assert_eq!(step(&order, Some(4), 1), Some(7));
    }

    #[test]
    fn shuffle_avoids_current_and_recent() {
        let order: Vec<usize> = (0..6).collect();
        let paths: Vec<PathBuf> = (0..6).map(|i| PathBuf::from(format!("/{i}"))).collect();
        let history = vec![paths[1].clone(), paths[2].clone(), paths[3].clone()];
        for seed in 0..200 {
            let next = pick(&order, &paths, Some(0), true, &history, seed).unwrap();
            assert!([4, 5].contains(&next), "picked {next}");
        }
    }

    #[test]
    fn shuffle_of_one_is_that_one() {
        let paths = vec![PathBuf::from("/0")];
        assert_eq!(pick(&[0], &paths, Some(0), true, &paths, 3), Some(0));
    }
}
