//! Stylesheets: the built-in one, the live accent, and the user's.

use gtk::gdk;
use gtk4 as gtk;
use hyprgallery_core::colour::Rgb;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

const DEFAULT: &str = include_str!("default.css");

pub struct Style {
    accent: gtk::CssProvider,
    user: gtk::CssProvider,
    user_path: PathBuf,
    current: Cell<Option<Rgb>>,
}

impl Style {
    pub fn install(display: &gdk::Display, user_path: PathBuf) -> Rc<Self> {
        // Above user priority, which is where ~/.config/gtk-4.0/gtk.css sits.
        // Desktop setups like HyDE put a whole theme there, and its button
        // and entry rules would otherwise restyle the gallery's own controls.
        // The gallery is its own surface with its own look; hyprgallery's
        // style.css is the place to change it.
        let base = gtk::CssProvider::new();
        base.load_from_string(DEFAULT);
        gtk::style_context_add_provider_for_display(
            display,
            &base,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 10,
        );

        // Above the defaults so the accent wins over their placeholder, below
        // the user's file so a stylesheet can still pin a colour of its own.
        let accent = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            display,
            &accent,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 11,
        );

        let user = gtk::CssProvider::new();
        user.connect_parsing_error(|_, section, error| {
            tracing::warn!("style.css {}: {error}", section.to_str());
        });
        gtk::style_context_add_provider_for_display(
            display,
            &user,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 12,
        );

        let style = Rc::new(Self { accent, user, user_path, current: Cell::new(None) });
        style.reload_user();
        style
    }

    pub fn reload_user(&self) {
        match std::fs::read_to_string(&self.user_path) {
            Ok(css) => self.user.load_from_string(&css),
            Err(_) => self.user.load_from_string(""),
        }
    }

    /// Recolour everything that uses `@hg_accent`. The CSS transitions on
    /// those widgets turn this into a fade.
    pub fn set_accent(&self, accent: Rgb) {
        if self.current.replace(Some(accent)) == Some(accent) {
            return;
        }
        self.accent.load_from_string(&format!(
            "@define-color hg_accent {};\n@define-color hg_accent_fg #141218;\n",
            accent.hex()
        ));
    }
}
