//! The two layouts' widget trees. They share one model and one selection, so
//! switching between them keeps your place.

use crate::backdrop::Backdrop;
use crate::motion::Motion;
use gtk::prelude::*;
use gtk4 as gtk;
use hyprgallery_core::Layout;
use hyprgallery_core::config::{Anchor, PopoverConfig};

pub struct Page {
    pub layout: Layout,
    /// What goes in the window's stack.
    pub root: gtk::Overlay,
    /// Faded and scaled on open, and while peeking.
    pub content: Motion,
    pub panel: gtk::Box,
    pub album_scroll: gtk::ScrolledWindow,
    pub albums: gtk::Box,
    pub search: gtk::SearchEntry,
    pub hues_revealer: gtk::Revealer,
    pub hues: gtk::Box,
    pub body: gtk::Stack,
    /// Faded when the album changes.
    pub grid_motion: Motion,
    pub grid: gtk::GridView,
    pub empty_title: gtk::Label,
    pub empty_hint: gtk::Label,
    pub status: gtk::Label,
    pub rotate: gtk::MenuButton,
}

impl Page {
    pub fn build(
        layout: Layout,
        selection: &gtk::SingleSelection,
        factory: &gtk::SignalListItemFactory,
        backdrop: Option<&Backdrop>,
    ) -> Self {
        let overlay = layout == Layout::Overlay;

        let albums = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        albums.add_css_class(if overlay { "hg-album-row" } else { "hg-shelf-row" });
        let album_scroll = gtk::ScrolledWindow::builder()
            .child(&albums)
            // Scrollable by wheel and touchpad, without a scrollbar drawn
            // under a row of pills.
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .build();
        album_scroll.add_css_class(if overlay { "hg-albums" } else { "hg-shelf" });

        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search all wallpapers")
            .valign(gtk::Align::Center)
            .build();
        search.add_css_class("hg-search");

        let hues = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hues.add_css_class("hg-hues");
        let hues_revealer = gtk::Revealer::builder()
            .child(&hues)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(220)
            .build();

        let grid = gtk::GridView::new(Some(selection.clone()), Some(factory.clone()));
        grid.add_css_class("hg-grid");
        grid.set_min_columns(2);
        grid.set_max_columns(if overlay { 8 } else { 4 });
        // Hover follows the pointer and a single click applies, which is what
        // makes the backdrop preview whatever is under the pointer.
        grid.set_single_click_activate(true);
        grid.set_enable_rubberband(false);
        let grid_scroll = gtk::ScrolledWindow::builder()
            .child(&grid)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        grid_scroll.add_css_class("hg-grid-scroll");
        let grid_motion = Motion::new(&grid_scroll, 0.99, 10.0, (0.5, 0.0));

        let empty_title = gtk::Label::new(None);
        empty_title.add_css_class("hg-empty-title");
        let empty_hint = gtk::Label::new(None);
        empty_hint.add_css_class("hg-empty-hint");
        empty_hint.set_wrap(true);
        empty_hint.set_justify(gtk::Justification::Center);
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
        empty.add_css_class("hg-empty");
        empty.set_valign(gtk::Align::Center);
        empty.append(&empty_title);
        empty.append(&empty_hint);

        let body = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(180)
            .vexpand(true)
            .build();
        body.add_named(&grid_motion, Some("grid"));
        body.add_named(&empty, Some("empty"));

        let status =
            gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).xalign(1.0).build();
        status.add_css_class("hg-status");
        let rotate =
            gtk::MenuButton::builder().label("Rotate").direction(gtk::ArrowType::Up).build();
        rotate.add_css_class("hg-rotate");
        rotate.set_tooltip_text(Some("Change the wallpaper on a timer (Ctrl+R)"));

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.add_css_class("hg-footer");
        if overlay {
            footer.append(&hints(&[
                ("\u{21B5}", "Apply"),
                ("Space", "Hold to preview"),
                ("F", "Favourite"),
                ("/", "Search"),
                ("Tab", "Next album"),
                ("Esc", "Close"),
            ]));
        }
        status.set_hexpand(true);
        footer.append(&status);
        footer.append(&rotate);

        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let root = gtk::Overlay::new();
        let content;
        if overlay {
            let header = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            header.add_css_class("hg-header");
            header.append(&album_scroll);
            header.append(&search);
            panel.add_css_class("hg-content-box");
            panel.append(&header);
            panel.append(&hues_revealer);
            panel.append(&body);
            panel.append(&footer);
            content = Motion::new(&panel, 0.965, 22.0, (0.5, 0.45));
            if let Some(backdrop) = backdrop {
                root.set_child(Some(backdrop));
            }
            root.add_overlay(&content);
        } else {
            let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            header.add_css_class("hg-header");
            search.set_hexpand(true);
            header.append(&search);
            panel.add_css_class("hg-panel");
            panel.append(&album_scroll);
            panel.append(&header);
            panel.append(&hues_revealer);
            panel.append(&body);
            panel.append(&footer);
            content = Motion::new(&panel, 0.96, -10.0, (1.0, 0.0));
            // The rest of the surface is transparent and catches the click that
            // dismisses the panel.
            let catcher = gtk::Box::new(gtk::Orientation::Vertical, 0);
            catcher.add_css_class("hg-catcher");
            root.set_child(Some(&catcher));
            root.add_overlay(&content);
        }

        Self {
            layout,
            root,
            content,
            panel,
            album_scroll,
            albums,
            search,
            hues_revealer,
            hues,
            body,
            grid_motion,
            grid,
            empty_title,
            empty_hint,
            status,
            rotate,
        }
    }

    /// Size and pin the popover panel as configured.
    pub fn place_panel(&self, config: &PopoverConfig) {
        if self.layout != Layout::Popover {
            return;
        }
        let (h, v, origin) = match config.anchor {
            Anchor::TopLeft => (gtk::Align::Start, gtk::Align::Start, (0.0, 0.0)),
            Anchor::Top => (gtk::Align::Center, gtk::Align::Start, (0.5, 0.0)),
            Anchor::TopRight => (gtk::Align::End, gtk::Align::Start, (1.0, 0.0)),
            Anchor::BottomLeft => (gtk::Align::Start, gtk::Align::End, (0.0, 1.0)),
            Anchor::Bottom => (gtk::Align::Center, gtk::Align::End, (0.5, 1.0)),
            Anchor::BottomRight => (gtk::Align::End, gtk::Align::End, (1.0, 1.0)),
        };
        self.content.set_halign(h);
        self.content.set_valign(v);
        self.content.set_origin(origin);
        self.content.set_margin_top(config.margin);
        self.content.set_margin_bottom(config.margin);
        self.content.set_margin_start(config.margin);
        self.content.set_margin_end(config.margin);
        self.panel.set_size_request(config.width, config.height);
    }

    pub fn search_focused(&self) -> bool {
        self.search.has_focus() || self.search.focus_child().is_some()
    }
}

fn hints(pairs: &[(&str, &str)]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for (key, what) in pairs {
        let k = gtk::Label::new(Some(key));
        k.add_css_class("hg-key");
        let w = gtk::Label::new(Some(what));
        w.add_css_class("hg-hint");
        row.append(&k);
        row.append(&w);
    }
    row
}
