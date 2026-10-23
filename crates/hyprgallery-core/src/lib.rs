//! Everything hyprgallery does that is not drawing.
//!
//! Kept free of GTK so the library scan, the thumbnail pipeline, the state file
//! and the backends can be tested on a machine with no display, and so a broken
//! system library can never be the reason the logic looks broken.

pub mod backend;
pub mod colour;
pub mod config;
pub mod decode;
pub mod engine;
pub mod ipc;
pub mod library;
pub mod naming;
pub mod paths;
pub mod rotation;
pub mod search;
pub mod state;
pub mod thumbs;
pub mod view;

pub use config::{Config, Layout};
pub use library::{Album, Kind, Library, Wallpaper};
pub use state::State;
pub use view::ViewId;

/// Seconds since the Unix epoch. Everything persisted uses this rather than
/// `SystemTime`, so the files stay readable and comparable by hand.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
