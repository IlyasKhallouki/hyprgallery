//! The gallery: a GTK4 layer-shell surface with two layouts, a full-screen
//! overlay and a popover that hangs from the bar.
//!
//! This crate draws and handles input. Everything it shows comes from a
//! [`hyprgallery_core::engine::Engine`] owned by the daemon, and everything it
//! changes goes back through that engine.

mod backdrop;
mod cover;
mod gallery;
mod motion;
mod page;
mod style;
mod textures;
mod tile;

pub use gallery::{Dispatch, Gallery, NAMESPACE};
pub use motion::time_scale;
