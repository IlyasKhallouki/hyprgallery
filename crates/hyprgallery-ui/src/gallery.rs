//! The gallery window: one layer surface holding both layouts, kept alive and
//! hidden between uses so opening it is only ever a fade.

use crate::backdrop::Backdrop;
use crate::motion::{self, Animated, Debounce, Motion};
use crate::page::Page;
use crate::style::Style;
use crate::textures::Loader;
use crate::tile::{Tile, TileData};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use hyprgallery_core::colour::Hue;
use hyprgallery_core::engine::{Change, Engine};
use hyprgallery_core::library::{Kind, Origin};
use hyprgallery_core::rotation::{self, Rotation};
use hyprgallery_core::view::{self, ViewId};
use hyprgallery_core::{Layout, search};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

/// Wayland namespace of the surface. Compositor rules match on it, so it is
/// documented and stable.
pub const NAMESPACE: &str = "hyprgallery";

/// How the gallery reports what it changed. The daemon passes the changes on
/// to the bar, and back into the gallery.
pub type Dispatch = Rc<dyn Fn(Vec<Change>)>;

/// What a menu action does with the gallery and its parameter.
type ActionFn = Box<dyn Fn(&Rc<Gallery>, Option<&glib::Variant>)>;

/// One grid entry. Indexes into the library, which is rebuilt rather than
/// edited, so the store is refilled whenever the library changes.
#[derive(Debug, Clone)]
struct Item {
    idx: usize,
    key: String,
    path: PathBuf,
}

pub struct Gallery {
    pub window: gtk::ApplicationWindow,
    engine: Rc<RefCell<Engine>>,
    dispatch: Dispatch,
    loader: Rc<Loader>,
    style: Rc<Style>,
    layer_shell: bool,
    root: Motion,
    root_progress: Rc<Animated>,
    stack: gtk::Stack,
    backdrop: Backdrop,
    pages: [Page; 2],
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    view: RefCell<ViewId>,
    query: RefCell<String>,
    hue: Cell<Option<Hue>>,
    layout: Cell<Layout>,
    shown: Cell<bool>,
    tiles: RefCell<HashMap<String, Vec<glib::WeakRef<Tile>>>>,
    focus_settle: Rc<Debounce>,
    dwell: Rc<Debounce>,
    toast_timer: Rc<Debounce>,
    toast: gtk::Revealer,
    toast_label: gtk::Label,
    drop_zone: gtk::Revealer,
    drop_label: gtk::Label,
    peek_progress: Rc<Animated>,
    grid_fade: Rc<Animated>,
    peeking: Cell<bool>,
    media: RefCell<Option<gtk::MediaFile>>,
    pointer: Cell<Option<(f64, f64)>>,
    by_pointer: Cell<bool>,
    menu_target: RefCell<Option<PathBuf>>,
    shuffle: Cell<bool>,
    refilling: Cell<bool>,
}

impl Gallery {
    pub fn build(
        app: &gtk::Application,
        engine: Rc<RefCell<Engine>>,
        dispatch: Dispatch,
    ) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("hyprgallery")
            .decorated(false)
            .build();
        // Named so the theme's window background can be made transparent; the
        // backdrop and the panel paint everything that should be seen.
        window.set_widget_name("hyprgallery");

        let layer_shell = gtk4_layer_shell::is_supported();
        if layer_shell {
            window.init_layer_shell();
            window.set_namespace(Some(NAMESPACE));
            window.set_layer(Layer::Overlay);
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
        } else {
            tracing::warn!("no layer-shell support; the gallery will open as an ordinary window");
            window.set_default_size(1600, 900);
        }

        let style =
            Style::install(&WidgetExt::display(&window), engine.borrow().paths.style_file.clone());

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        selection.set_autoselect(true);
        selection.set_can_unselect(false);

        let backdrop = Backdrop::default();
        let factories = [gtk::SignalListItemFactory::new(), gtk::SignalListItemFactory::new()];
        let pages = [
            Page::build(Layout::Overlay, &selection, &factories[0], Some(&backdrop)),
            Page::build(Layout::Popover, &selection, &factories[1], None),
        ];

        let stack = gtk::Stack::new();
        for page in &pages {
            stack.add_child(&page.root);
        }

        let toast_label = gtk::Label::new(None);
        let toast = gtk::Revealer::builder()
            .child(&toast_label)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(220)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .can_target(false)
            .build();
        toast_label.add_css_class("hg-toast");

        let drop_label = gtk::Label::new(Some("Drop to add"));
        let drop_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        drop_box.add_css_class("hg-drop");
        drop_label.set_vexpand(true);
        drop_box.append(&drop_label);
        let drop_zone = gtk::Revealer::builder()
            .child(&drop_box)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(160)
            .can_target(false)
            .build();

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&drop_zone);
        overlay.add_overlay(&toast);
        let root = Motion::new(&overlay, 1.0, 0.0, (0.5, 0.5));
        window.set_child(Some(&root));

        let view = engine.borrow().view_for(None).unwrap_or(ViewId::Recent);
        let view = engine.borrow().state.view.clone().unwrap_or(view);
        let layout = engine.borrow().config.layout;
        let gallery = Rc::new(Self {
            window,
            engine,
            dispatch,
            loader: Loader::new(),
            style,
            layer_shell,
            root,
            root_progress: Animated::new(0.0),
            stack,
            backdrop,
            pages,
            store,
            selection,
            view: RefCell::new(view),
            query: RefCell::new(String::new()),
            hue: Cell::new(None),
            layout: Cell::new(layout),
            shown: Cell::new(false),
            tiles: RefCell::new(HashMap::new()),
            focus_settle: Rc::new(Debounce::default()),
            dwell: Rc::new(Debounce::default()),
            toast_timer: Rc::new(Debounce::default()),
            toast,
            toast_label,
            drop_zone,
            drop_label,
            peek_progress: Animated::new(1.0),
            grid_fade: Animated::new(1.0),
            peeking: Cell::new(false),
            media: RefCell::new(None),
            pointer: Cell::new(None),
            by_pointer: Cell::new(false),
            menu_target: RefCell::new(None),
            shuffle: Cell::new(false),
            refilling: Cell::new(false),
        });

        for (factory, min_width) in factories.iter().zip([262, 200]) {
            gallery.wire_factory(factory, min_width);
        }
        for i in 0..2 {
            gallery.wire_page(i);
        }
        gallery.wire_window();
        gallery.wire_actions();
        gallery.reconfigure();
        gallery.rebuild_albums();
        gallery.refill(None, false);
        gallery
    }

    fn page(&self) -> &Page {
        &self.pages[if self.layout.get() == Layout::Overlay { 0 } else { 1 }]
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    pub fn is_shown(&self) -> bool {
        self.shown.get()
    }

    // ----- showing and hiding ------------------------------------------------

    pub fn toggle(self: &Rc<Self>, layout: Option<Layout>) {
        let wanted = layout.unwrap_or_else(|| self.engine.borrow().config.layout);
        if self.shown.get() && (layout.is_none() || wanted == self.layout.get()) {
            self.hide();
        } else {
            self.show(Some(wanted));
        }
    }

    pub fn show(self: &Rc<Self>, layout: Option<Layout>) {
        let layout = layout.unwrap_or_else(|| self.engine.borrow().config.layout);
        // Changing the surface's exclusive zone needs a fresh map. Checked
        // against the window rather than `shown`, which is already false while
        // a close is still fading out.
        if self.window.is_visible() && layout != self.layout.get() {
            self.root_progress.set(0.0);
            self.window.set_visible(false);
            self.shown.set(false);
        }
        if self.shown.get() {
            return;
        }
        self.layout.set(layout);
        self.configure_surface(layout);
        let page = self.page();
        self.stack.set_visible_child(&page.root);
        self.shown.set(true);
        self.engine.borrow_mut().visible = true;

        // Open on whatever is on screen now, inside the album it came from.
        // Something else may have changed the wallpaper since last time.
        let synced = self.engine.borrow_mut().sync_current();
        if !synced.is_empty() {
            (self.dispatch)(synced);
        }
        let current = self.engine.borrow().state.current.clone();
        self.select_path(current.as_deref(), true);
        self.settle_focus(false);

        if layout == Layout::Overlay {
            self.root.configure(1.0, 0.0, (0.5, 0.5));
        } else {
            self.root.configure(0.96, -12.0, page.content.origin());
        }
        self.root.set_progress(0.0);
        self.root_progress.set(0.0);
        page.content.set_progress(if layout == Layout::Overlay { 0.0 } else { 1.0 });
        self.peek_progress.set(1.0);
        let asked = std::time::Instant::now();
        self.window.present();
        // How long from asking to the first frame on screen. The number that
        // decides whether opening feels instant.
        if let Some(clock) = self.window.frame_clock() {
            let handler = std::rc::Rc::new(std::cell::Cell::new(None::<glib::SignalHandlerId>));
            let h = std::rc::Rc::clone(&handler);
            let id = clock.connect_after_paint(move |clock| {
                tracing::debug!(
                    "first frame {:.1} ms after opening",
                    asked.elapsed().as_secs_f64() * 1000.0
                );
                if let Some(id) = h.take() {
                    clock.disconnect(id);
                }
            });
            handler.set(Some(id));
        } else {
            tracing::debug!("present took {:.1} ms", asked.elapsed().as_secs_f64() * 1000.0);
        }
        page.grid.grab_focus();

        let weak = self.weak();
        self.root_progress.animate(
            &self.window,
            1.0,
            if layout == Layout::Overlay { 300 } else { 220 },
            motion::ease_out_quint,
            move |p| {
                if let Some(g) = weak.upgrade() {
                    g.root.set_progress(p);
                    if g.layout.get() == Layout::Overlay {
                        g.page().content.set_progress(p);
                    }
                }
            },
            || {},
        );
        (self.dispatch)(Vec::new());
    }

    /// Start the GPU renderer and compile its shaders now, while nobody is
    /// waiting, instead of on the first click.
    ///
    /// The first frame a GTK window ever draws pays for loading the graphics
    /// driver and compiling every shader it touches, which is seconds on a
    /// busy machine. Realising the hidden window creates its renderer without
    /// showing anything (the surface gets no role or buffer until it is
    /// mapped), and drawing one small offscreen scene with every kind of node
    /// the gallery uses compiles the shaders it will need.
    pub fn warm_up(&self) {
        use gtk::{graphene, gsk};
        let started = std::time::Instant::now();
        WidgetExt::realize(&self.window);
        let Some(renderer) = self.window.native().and_then(|n| n.renderer()) else { return };
        let rect = graphene::Rect::new(0.0, 0.0, 64.0, 36.0);
        let snapshot = gtk::Snapshot::new();
        snapshot.append_color(&gdk::RGBA::BLACK, &rect);
        snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, 8.0));
        snapshot.push_opacity(0.5);
        let pixels = hyprgallery_core::decode::Rgb8 { width: 4, height: 4, data: vec![128; 48] };
        let texture = crate::textures::memory_texture(&pixels);
        snapshot.append_scaled_texture(&texture, gsk::ScalingFilter::Linear, &rect);
        let stops = [
            gsk::ColorStop::new(0.0, gdk::RGBA::new(0.0, 0.0, 0.0, 0.0)),
            gsk::ColorStop::new(1.0, gdk::RGBA::new(0.0, 0.0, 0.0, 0.6)),
        ];
        snapshot.append_linear_gradient(
            &rect,
            &graphene::Point::new(0.0, 0.0),
            &graphene::Point::new(0.0, 36.0),
            &stops,
        );
        snapshot.append_radial_gradient(
            &rect,
            &graphene::Point::new(32.0, 18.0),
            30.0,
            30.0,
            0.0,
            1.0,
            &stops,
        );
        let layout = self.window.create_pango_layout(Some("Warm up"));
        snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
        snapshot.append_outset_shadow(
            &gsk::RoundedRect::from_rect(rect, 8.0),
            &gdk::RGBA::BLACK,
            0.0,
            3.0,
            0.0,
            8.0,
        );
        snapshot.pop();
        snapshot.pop();
        if let Some(node) = snapshot.to_node() {
            let _ = renderer.render_texture(&node, Some(&rect));
        }
        tracing::debug!("renderer ready in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    }

    pub fn hide(self: &Rc<Self>) {
        if !self.shown.get() {
            return;
        }
        self.shown.set(false);
        self.engine.borrow_mut().visible = false;
        self.end_peek();
        self.stop_media();
        self.dwell.cancel();
        let weak = self.weak();
        self.root_progress.animate(
            &self.window,
            0.0,
            150,
            motion::ease_in_cubic,
            {
                let weak = weak.clone();
                move |p| {
                    if let Some(g) = weak.upgrade() {
                        g.root.set_progress(p);
                    }
                }
            },
            move || {
                if let Some(g) = weak.upgrade()
                    && !g.shown.get()
                {
                    g.window.set_visible(false);
                }
            },
        );
        (self.dispatch)(Vec::new());
    }

    fn configure_surface(&self, layout: Layout) {
        if !self.layer_shell {
            self.window.set_visible(true);
            return;
        }
        // Full screen covers the bar too; the popover keeps clear of it so the
        // bar stays clickable, which is how a second click on the module
        // closes the panel again.
        self.window.set_exclusive_zone(if layout == Layout::Overlay { -1 } else { 0 });
        // Exclusive keyboard while open: search and shortcuts work at once,
        // without first clicking into the gallery.
        self.window.set_keyboard_mode(KeyboardMode::Exclusive);
    }

    /// Config changed: popover size and position.
    pub fn reconfigure(&self) {
        let config = self.engine.borrow().config.popover.clone();
        self.pages[1].place_panel(&config);
    }

    pub fn reload_style(&self) {
        self.style.reload_user();
    }

    // ----- the grid ----------------------------------------------------------

    fn wire_factory(self: &Rc<Self>, factory: &gtk::SignalListItemFactory, min_width: i32) {
        let weak = self.weak();
        factory.connect_setup(move |_, obj| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let tile = Tile::new(min_width);
            item.set_child(Some(&tile));
            let tile_weak = tile.downgrade();
            item.connect_selected_notify(move |item| {
                if let Some(tile) = tile_weak.upgrade() {
                    tile.set_selected(item.is_selected());
                }
            });
            // Right click: the menu for this tile.
            let click = gtk::GestureClick::builder().button(3).build();
            let weak = weak.clone();
            let item_weak = item.downgrade();
            click.connect_pressed(move |gesture, _, x, y| {
                let (Some(g), Some(item)) = (weak.upgrade(), item_weak.upgrade()) else { return };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                g.selection.set_selected(item.position());
                if let Some(tile) = item.child().and_downcast::<Tile>() {
                    g.open_tile_menu(&tile, Some((x, y)));
                }
            });
            tile.add_controller(click);
        });

        let weak = self.weak();
        factory.connect_bind(move |_, obj| {
            let (Some(g), Some(item)) = (weak.upgrade(), obj.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            let (Some(tile), Some(data)) = (
                item.child().and_downcast::<Tile>(),
                item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) else {
                return;
            };
            let entry = data.borrow::<Item>().clone();
            g.bind_tile(&tile, &entry);
            tile.set_selected_now(item.is_selected());
        });

        let weak = self.weak();
        factory.connect_unbind(move |_, obj| {
            let (Some(g), Some(item)) = (weak.upgrade(), obj.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            if let Some(tile) = item.child().and_downcast::<Tile>() {
                tile.stop_preview();
                let key = tile.key();
                let mut tiles = g.tiles.borrow_mut();
                if let Some(list) = tiles.get_mut(&key) {
                    list.retain(|w| w.upgrade().is_some_and(|t| t != tile));
                    if list.is_empty() {
                        tiles.remove(&key);
                    }
                }
            }
        });
    }

    fn tile_data(&self, item: &Item) -> TileData {
        let engine = self.engine.borrow();
        let Some(w) = engine.library.wallpapers.get(item.idx).filter(|w| w.path == item.path)
        else {
            return TileData { key: item.key.clone(), ..TileData::default() };
        };
        let meta = engine.metas.get(&w.key);
        let colour = meta.map(|m| {
            let c = m.colour;
            gdk::RGBA::new(
                c.0 as f32 / 255.0 * 0.7,
                c.1 as f32 / 255.0 * 0.7,
                c.2 as f32 / 255.0 * 0.7,
                1.0,
            )
        });
        let badge = match w.kind {
            Kind::Video => Some("VIDEO"),
            Kind::Animated if meta.is_none_or(|m| m.animated) => Some("GIF"),
            _ => None,
        };
        TileData {
            key: w.key.clone(),
            name: w.name.clone(),
            colour,
            favourite: engine.state.is_favourite(&w.path),
            current: engine.state.current.as_deref() == Some(w.path.as_path()),
            applying: engine.is_applying(&w.path),
            failed: engine.failed(&w.key),
            badge,
        }
    }

    fn bind_tile(self: &Rc<Self>, tile: &Tile, item: &Item) {
        let data = self.tile_data(item);
        tile.set_data(&data);
        self.tiles.borrow_mut().entry(item.key.clone()).or_default().push(tile.downgrade());
        self.load_thumb(tile, item);
    }

    fn load_thumb(self: &Rc<Self>, tile: &Tile, item: &Item) {
        if tile.has_thumb() {
            return;
        }
        if let Some(texture) = self.loader.cached_thumb(&item.key) {
            tile.set_thumb(texture, false);
            return;
        }
        let (ready, path) = {
            let engine = self.engine.borrow();
            (engine.metas.ready(&engine.paths.cache_dir, &item.key), engine.thumb_path(&item.key))
        };
        if !ready {
            // Still being made. Its Change::Meta brings us back here.
            return;
        }
        let tile_weak = tile.downgrade();
        let key = item.key.clone();
        self.loader.thumb(&item.key, path, move |texture| {
            if let (Some(tile), Some(texture)) = (tile_weak.upgrade(), texture)
                && tile.key() == key
            {
                tile.set_thumb(texture, true);
            }
        });
    }

    fn bound_tiles(&self, key: &str) -> Vec<Tile> {
        self.tiles
            .borrow()
            .get(key)
            .map(|l| l.iter().filter_map(|w| w.upgrade()).collect())
            .unwrap_or_default()
    }

    fn all_bound(&self) -> Vec<(String, Tile)> {
        self.tiles
            .borrow()
            .iter()
            .flat_map(|(k, l)| l.iter().filter_map(|w| w.upgrade()).map(move |t| (k.clone(), t)))
            .collect()
    }

    fn selected_item(&self) -> Option<Item> {
        self.selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|o| o.borrow::<Item>().clone())
    }

    fn item_at(&self, pos: u32) -> Option<Item> {
        self.store
            .item(pos)
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|o| o.borrow::<Item>().clone())
    }

    /// Fill the grid for the current view, search and colour filter.
    fn refill(self: &Rc<Self>, keep: Option<PathBuf>, animate: bool) {
        if animate && self.shown.get() {
            // The fade lives in a field: an animation only holds a weak
            // reference to its value, so a local would be dropped before its
            // first frame and the refill at the end would never run. One
            // shared value also means a second quick switch takes over the
            // fade instead of stacking another refill on top.
            let weak = self.weak();
            let grid_motion = self.page().grid_motion.clone();
            let gm = grid_motion.clone();
            self.grid_fade.animate(
                &grid_motion,
                0.0,
                90,
                motion::ease_in_cubic,
                move |p| gm.set_progress(p),
                move || {
                    let Some(g) = weak.upgrade() else { return };
                    g.refill(keep, false);
                    let gm = g.page().grid_motion.clone();
                    let gm2 = gm.clone();
                    g.grid_fade.animate(
                        &gm,
                        1.0,
                        220,
                        motion::ease_out_cubic,
                        move |p| gm2.set_progress(p),
                        || {},
                    );
                },
            );
            return;
        }

        let query = self.query.borrow().trim().to_owned();
        let view = self.view.borrow().clone();
        let items: Vec<glib::BoxedAnyObject> = {
            let engine = self.engine.borrow();
            let mut indices = if query.is_empty() {
                engine.order(&view)
            } else {
                search::search(&engine.library, &query, 400)
            };
            if query.is_empty()
                && view == ViewId::Colour
                && let Some(hue) = self.hue.get()
            {
                indices.retain(|&i| {
                    engine
                        .metas
                        .get(&engine.library.wallpapers[i].key)
                        .is_some_and(|m| m.hue == hue)
                });
            }
            indices
                .into_iter()
                .map(|idx| {
                    let w = &engine.library.wallpapers[idx];
                    glib::BoxedAnyObject::new(Item {
                        idx,
                        key: w.key.clone(),
                        path: w.path.clone(),
                    })
                })
                .collect()
        };

        self.refilling.set(true);
        self.store.splice(0, self.store.n_items(), &items);
        self.refilling.set(false);
        let keep = keep.or_else(|| self.engine.borrow().state.current.clone());
        let pos = keep.and_then(|k| self.position_of(&k)).unwrap_or(0);
        if self.store.n_items() > 0 {
            self.selection.set_selected(pos);
            for page in &self.pages {
                page.grid.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
            }
        }
        self.update_empty(&view, &query);
        self.mark_active_album();
        self.settle_focus(false);
    }

    fn position_of(&self, path: &Path) -> Option<u32> {
        (0..self.store.n_items()).find(|&i| self.item_at(i).is_some_and(|it| it.path == path))
    }

    /// Select `path`, switching to its album if it is not in the current view.
    fn select_path(self: &Rc<Self>, path: Option<&Path>, switch_view: bool) {
        let Some(path) = path else {
            return;
        };
        if let Some(pos) = self.position_of(path) {
            self.selection.set_selected(pos);
            self.page().grid.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
            return;
        }
        if switch_view {
            let album = {
                let engine = self.engine.borrow();
                engine.library.find(path).map(|i| {
                    ViewId::Album(engine.library.albums[engine.library.wallpapers[i].album].id())
                })
            };
            if let Some(view) = album {
                self.query.replace(String::new());
                for page in &self.pages {
                    page.search.set_text("");
                }
                self.hue.set(None);
                self.view.replace(view.clone());
                self.engine.borrow_mut().set_view(view);
                self.rebuild_hues();
                self.update_rotate();
                self.refill(Some(path.to_owned()), false);
            }
        }
    }

    fn update_empty(&self, view: &ViewId, query: &str) {
        let empty = self.store.n_items() == 0;
        let (title, hint) = {
            let engine = self.engine.borrow();
            if !query.is_empty() {
                (
                    format!("Nothing matches \u{201C}{query}\u{201D}"),
                    "Search looks at names, albums and file names.".to_owned(),
                )
            } else if engine.library.wallpapers.is_empty() {
                let root = engine.config.library.roots.first().cloned().unwrap_or_default();
                (
                    "No wallpapers yet".to_owned(),
                    format!(
                        "Drop images here, or put some in {root}.\nEach folder inside it becomes an album."
                    ),
                )
            } else if *view == ViewId::Favourites {
                (
                    "No favourites yet".to_owned(),
                    "Press F on a wallpaper to keep it here.".to_owned(),
                )
            } else {
                ("This album is empty".to_owned(), String::new())
            }
        };
        for page in &self.pages {
            page.body.set_visible_child_name(if empty { "empty" } else { "grid" });
            page.empty_title.set_text(&title);
            page.empty_hint.set_text(&hint);
            page.empty_hint.set_visible(!hint.is_empty());
        }
    }

    // ----- albums ------------------------------------------------------------

    /// Rebuild both album rows. Cheap: a few dozen buttons.
    pub fn rebuild_albums(self: &Rc<Self>) {
        let views = self.engine.borrow().views();
        for (i, page) in self.pages.iter().enumerate() {
            while let Some(child) = page.albums.first_child() {
                page.albums.remove(&child);
            }
            for (n, info) in views.iter().enumerate() {
                // A divider between the smart albums and your folders.
                if i == 0 && !info.smart && n > 0 && views[n - 1].smart {
                    let sep = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    sep.add_css_class("hg-separator");
                    page.albums.append(&sep);
                }
                let button = if i == 0 { self.album_pill(info) } else { self.album_cover(info) };
                let weak = self.weak();
                let id = info.id.clone();
                button.connect_clicked(move |_| {
                    if let Some(g) = weak.upgrade() {
                        g.set_view(id.clone());
                    }
                });
                // Remember which view each button is for, to mark the active one.
                unsafe { button.set_data("hg-view", info.id.clone()) };
                page.albums.append(&button);
            }
        }
        self.mark_active_album();
        self.rebuild_hues();
        self.update_rotate();
    }

    fn album_pill(&self, info: &view::ViewInfo) -> gtk::Button {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let label = gtk::Label::new(Some(&format!(
            "{}{}",
            glyph(&info.id).map(|g| format!("{g}  ")).unwrap_or_default(),
            info.label
        )));
        label.add_css_class("hg-album-name");
        let count = gtk::Label::new(Some(&info.count.to_string()));
        count.add_css_class("hg-album-count");
        row.append(&label);
        if !matches!(info.id, ViewId::Colour) {
            row.append(&count);
        }
        let button = gtk::Button::builder().child(&row).focus_on_click(false).build();
        button.add_css_class("hg-album");
        if info.smart {
            button.add_css_class("smart");
        }
        button
    }

    fn album_cover(self: &Rc<Self>, info: &view::ViewInfo) -> gtk::Button {
        let cover = crate::cover::Cover::default();
        let tint = match info.id {
            ViewId::Favourites => Some(gdk::RGBA::new(0.85, 0.32, 0.45, 1.0)),
            ViewId::Recent => Some(gdk::RGBA::new(0.3, 0.42, 0.75, 1.0)),
            ViewId::Colour => Some(gdk::RGBA::new(0.55, 0.4, 0.8, 1.0)),
            ViewId::Animated => Some(gdk::RGBA::new(0.25, 0.6, 0.55, 1.0)),
            ViewId::Album(_) => None,
        };
        let firsts: Vec<(String, PathBuf)> = {
            let engine = self.engine.borrow();
            engine
                .order(&info.id)
                .into_iter()
                .filter(|&i| {
                    engine.metas.ready(&engine.paths.cache_dir, &engine.library.wallpapers[i].key)
                })
                .take(3)
                .map(|i| {
                    let key = engine.library.wallpapers[i].key.clone();
                    let path = engine.thumb_path(&key);
                    (key, path)
                })
                .collect()
        };
        // Smart albums show their symbol, over their thumbnails when they
        // have any and over a coloured card when they do not.
        cover.set_glyph(glyph(&info.id), tint);
        let collected: Rc<RefCell<Vec<Option<gdk::Texture>>>> =
            Rc::new(RefCell::new(vec![None; firsts.len()]));
        for (n, (key, path)) in firsts.iter().enumerate() {
            let collected = Rc::clone(&collected);
            let cover_weak = cover.downgrade();
            self.loader.thumb(key, path.clone(), move |texture| {
                collected.borrow_mut()[n] = texture;
                if let Some(cover) = cover_weak.upgrade() {
                    // Reverse, so the first wallpaper is the card on top.
                    let thumbs: Vec<gdk::Texture> =
                        collected.borrow().iter().rev().flatten().cloned().collect();
                    cover.set_thumbs(thumbs);
                }
            });
        }
        let label = gtk::Label::builder()
            .label(&info.label)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&cover);
        column.append(&label);
        let button = gtk::Button::builder().child(&column).focus_on_click(false).build();
        button.set_tooltip_text(Some(&format!("{} ({})", info.label, info.count)));
        button.add_css_class("hg-shelf-item");
        button
    }

    fn mark_active_album(&self) {
        let view = self.view.borrow().clone();
        let searching = !self.query.borrow().trim().is_empty();
        for page in &self.pages {
            let mut child = page.albums.first_child();
            while let Some(widget) = child {
                let id: Option<&ViewId> =
                    unsafe { widget.data::<ViewId>("hg-view").map(|p| p.as_ref()) };
                if id.is_some_and(|id| *id == view) && !searching {
                    widget.add_css_class("active");
                    let page_scroll = page.album_scroll.clone();
                    let w = widget.clone();
                    // Once laid out, bring the active album into view.
                    glib::idle_add_local_once(move || scroll_into_view(&page_scroll, &w));
                } else {
                    widget.remove_css_class("active");
                }
                child = widget.next_sibling();
            }
            page.hues_revealer.set_reveal_child(view == ViewId::Colour && !searching);
        }
    }

    fn rebuild_hues(self: &Rc<Self>) {
        let present: Vec<(Hue, usize)> = {
            let engine = self.engine.borrow();
            Hue::ALL
                .iter()
                .map(|&h| {
                    (
                        h,
                        engine
                            .library
                            .wallpapers
                            .iter()
                            .filter(|w| engine.metas.get(&w.key).is_some_and(|m| m.hue == h))
                            .count(),
                    )
                })
                .filter(|(_, n)| *n > 0)
                .collect()
        };
        for page in &self.pages {
            while let Some(child) = page.hues.first_child() {
                page.hues.remove(&child);
            }
            let all = gtk::Button::with_label("All");
            all.add_css_class("hg-hue");
            if self.hue.get().is_none() {
                all.add_css_class("active");
            }
            let weak = self.weak();
            all.connect_clicked(move |_| {
                if let Some(g) = weak.upgrade() {
                    g.set_hue(None);
                }
            });
            page.hues.append(&all);
            for (hue, _) in &present {
                let label = gtk::Label::new(None);
                label.set_markup(&format!(
                    "<span foreground=\"{}\" size=\"large\">\u{25CF}</span> {}",
                    hue.swatch().hex(),
                    hue.label()
                ));
                let button = gtk::Button::builder().child(&label).focus_on_click(false).build();
                button.add_css_class("hg-hue");
                if self.hue.get() == Some(*hue) {
                    button.add_css_class("active");
                }
                let weak = self.weak();
                let hue = *hue;
                button.connect_clicked(move |_| {
                    if let Some(g) = weak.upgrade() {
                        g.set_hue(Some(hue));
                    }
                });
                page.hues.append(&button);
            }
        }
    }

    fn set_hue(self: &Rc<Self>, hue: Option<Hue>) {
        self.hue.set(hue);
        self.rebuild_hues();
        self.refill(None, true);
    }

    pub fn set_view(self: &Rc<Self>, view: ViewId) {
        let searching = !self.query.borrow().is_empty();
        if *self.view.borrow() == view && !searching {
            return;
        }
        self.query.replace(String::new());
        for page in &self.pages {
            if !page.search.text().is_empty() {
                page.search.set_text("");
            }
        }
        self.hue.set(None);
        self.view.replace(view.clone());
        self.engine.borrow_mut().set_view(view);
        self.rebuild_hues();
        self.update_rotate();
        self.refill(None, true);
    }

    /// Select a view by what someone would call it. For scripted snapshots.
    pub fn set_view_named(self: &Rc<Self>, name: &str) -> bool {
        let view = ViewId::parse(&self.engine.borrow().library, name);
        match view {
            Some(v) => {
                self.set_view(v);
                true
            }
            None => false,
        }
    }

    fn step_album(self: &Rc<Self>, delta: isize) {
        let views = self.engine.borrow().views();
        if views.is_empty() {
            return;
        }
        let current = views.iter().position(|v| v.id == *self.view.borrow()).unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(views.len() as isize) as usize;
        self.set_view(views[next].id.clone());
    }

    // ----- focus, backdrop and peek ------------------------------------------

    /// Selection moved. The backdrop follows once the pointer or keyboard
    /// pauses, so sweeping across a row does not start a dozen crossfades.
    fn settle_focus(self: &Rc<Self>, debounce: bool) {
        if self.refilling.get() {
            return;
        }
        self.stop_media();
        let weak = self.weak();
        let run = move || {
            if let Some(g) = weak.upgrade() {
                g.focus_settled();
            }
        };
        if debounce {
            self.focus_settle.run(70, run);
        } else {
            self.focus_settle.cancel();
            run();
        }
    }

    fn focus_settled(self: &Rc<Self>) {
        let Some(item) = self.selected_item() else {
            self.update_status();
            return;
        };
        let meta = self.engine.borrow().metas.get(&item.key).cloned();
        if let Some(meta) = &meta {
            self.style.set_accent(meta.accent());
            let animate = self.shown.get();
            let backdrop = self.backdrop.downgrade();
            let key = item.key.clone();
            let weak = self.weak();
            self.loader.backdrop(&item.key, meta, move |texture| {
                let (Some(backdrop), Some(g)) = (backdrop.upgrade(), weak.upgrade()) else {
                    return;
                };
                // Only if focus is still there by the time it is ready.
                if g.selected_item().is_some_and(|i| i.key == key) {
                    backdrop.show(texture.map(|t| t.upcast()), animate);
                }
            });
        }
        self.update_status();
        if self.peeking.get() {
            self.update_peek(&item);
        }

        // After a short dwell, warm the full-size peek image and start any
        // animated preview. Not before: these cost real decoding.
        let weak = self.weak();
        self.dwell.run(260, move || {
            let Some(g) = weak.upgrade() else { return };
            if g.selected_item().is_none_or(|i| i.key != item.key) || !g.shown.get() {
                return;
            }
            let kind = g.engine.borrow().library.wallpapers.get(item.idx).map(|w| w.kind);
            match kind {
                Some(Kind::Video | Kind::Animated) => g.play_media(&item),
                Some(kind) => {
                    g.loader.peek(&item.key, item.path.clone(), kind, g.peek_size(), |_| {})
                }
                None => {}
            }
        });
    }

    fn peek_size(&self) -> (u32, u32) {
        let scale =
            self.window.native().and_then(|n| n.surface()).map(|s| s.scale()).unwrap_or(1.0);
        let (w, h) =
            (self.window.width().max(1) as f64 * scale, self.window.height().max(1) as f64 * scale);
        (w.round() as u32, h.round() as u32)
    }

    fn play_media(self: &Rc<Self>, item: &Item) {
        let media = gtk::MediaFile::for_filename(&item.path);
        media.set_loop(true);
        media.set_muted(true);
        media.play();
        for tile in self.bound_tiles(&item.key) {
            tile.play_preview(media.clone().upcast());
        }
        if self.peeking.get() {
            self.backdrop.set_peek_paintable(Some(media.clone().upcast()));
        }
        self.media.replace(Some(media));
    }

    fn stop_media(&self) {
        if let Some(media) = self.media.borrow_mut().take() {
            media.pause();
        }
    }

    pub fn start_peek(self: &Rc<Self>) {
        if self.peeking.replace(true) || self.layout.get() != Layout::Overlay {
            if self.layout.get() != Layout::Overlay {
                self.peeking.set(false);
            }
            return;
        }
        if let Some(item) = self.selected_item() {
            self.update_peek(&item);
        }
        self.backdrop.set_peeking(true);
        let content = self.page().content.clone();
        self.peek_progress.animate(
            &content,
            0.0,
            200,
            motion::ease_out_cubic,
            {
                let content = content.clone();
                move |p| content.set_progress(p)
            },
            || {},
        );
    }

    fn update_peek(self: &Rc<Self>, item: &Item) {
        if let Some(media) = self.media.borrow().as_ref() {
            self.backdrop.set_peek_paintable(Some(media.clone().upcast()));
            return;
        }
        // The thumbnail at once, so there is never a blank frame; the sharp
        // version replaces it when it is decoded.
        if let Some(thumb) = self.loader.cached_thumb(&item.key) {
            self.backdrop.set_peek_paintable(Some(thumb.upcast()));
        }
        let kind = self
            .engine
            .borrow()
            .library
            .wallpapers
            .get(item.idx)
            .map(|w| w.kind)
            .unwrap_or(Kind::Still);
        if kind == Kind::Still {
            let weak = self.weak();
            let key = item.key.clone();
            self.loader.peek(
                &item.key,
                item.path.clone(),
                kind,
                self.peek_size(),
                move |texture| {
                    let Some(g) = weak.upgrade() else { return };
                    if g.peeking.get()
                        && g.selected_item().is_some_and(|i| i.key == key)
                        && let Some(t) = texture
                    {
                        g.backdrop.set_peek_paintable(Some(t.upcast()));
                    }
                },
            );
        }
    }

    pub fn end_peek(self: &Rc<Self>) {
        if !self.peeking.replace(false) {
            return;
        }
        self.backdrop.set_peeking(false);
        let content = self.page().content.clone();
        self.peek_progress.animate(
            &content,
            1.0,
            240,
            motion::ease_out_cubic,
            {
                let content = content.clone();
                move |p| content.set_progress(p)
            },
            || {},
        );
    }

    fn update_status(&self) {
        let engine = self.engine.borrow();
        let (text, busy) = if let Some((done, total)) = engine.status().indexing {
            (format!("Preparing thumbnails  {done} of {total}"), true)
        } else if engine.status().applying {
            let slow = engine.backend().is_ok_and(|b| b.is_slow());
            (
                if slow {
                    "Applying, and recolouring the theme".to_owned()
                } else {
                    "Applying".to_owned()
                },
                true,
            )
        } else if let Some(item) = self.selected_item()
            && let Some(w) = engine.library.wallpapers.get(item.idx)
        {
            let album = &engine.library.albums[w.album].name;
            let size = engine
                .metas
                .get(&w.key)
                .map(|m| format!("  \u{00B7}  {}\u{00D7}{}", m.width, m.height))
                .unwrap_or_default();
            (format!("{}  \u{00B7}  {album}{size}", w.name), false)
        } else {
            (String::new(), false)
        };
        for page in &self.pages {
            page.status.set_text(&text);
            if busy {
                page.status.add_css_class("busy");
            } else {
                page.status.remove_css_class("busy");
            }
        }
    }

    // ----- applying ----------------------------------------------------------

    fn apply_item(self: &Rc<Self>, item: &Item) {
        let at = self.transition_origin(item);
        let changes = {
            let mut engine = self.engine.borrow_mut();
            let transition = engine.transition(at);
            engine.apply(&item.path, Some(transition))
        };
        (self.dispatch)(changes);
        if self.engine.borrow().config.close_on_apply {
            self.hide();
        }
    }

    /// Where the change should grow from: the click, or the tile chosen with
    /// the keyboard. Only meaningful when the surface covers the whole output,
    /// which is the overlay; the popover leaves it to the backend's default.
    fn transition_origin(&self, item: &Item) -> Option<(i32, i32)> {
        if self.layout.get() != Layout::Overlay || !self.layer_shell {
            return None;
        }
        let scale =
            self.window.native().and_then(|n| n.surface()).map(|s| s.scale()).unwrap_or(1.0);
        let point = if self.by_pointer.get() {
            self.pointer.get()
        } else {
            self.bound_tiles(&item.key).into_iter().find(|t| t.is_mapped()).and_then(|tile| {
                let centre = gtk::graphene::Point::new(
                    tile.width() as f32 / 2.0,
                    tile.height() as f32 / 2.0,
                );
                tile.compute_point(&self.window, &centre).map(|p| (p.x() as f64, p.y() as f64))
            })
        };
        point.map(|(x, y)| ((x * scale).round() as i32, (y * scale).round() as i32))
    }

    // ----- wiring ------------------------------------------------------------

    fn wire_page(self: &Rc<Self>, i: usize) {
        let page = &self.pages[i];

        // A mouse wheel only scrolls vertically, and the album row only
        // scrolls sideways. Turn one into the other.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let scroller = page.album_scroll.clone();
        wheel.connect_scroll(move |c, _, dy| {
            let adj = scroller.hadjustment();
            let step = if c.unit() == gdk::ScrollUnit::Wheel { 60.0 } else { 1.0 };
            adj.set_value(adj.value() + dy * step);
            glib::Propagation::Stop
        });
        page.album_scroll.add_controller(wheel);

        let weak = self.weak();
        page.grid.connect_activate(move |_, pos| {
            if let Some(g) = weak.upgrade()
                && let Some(item) = g.item_at(pos)
            {
                g.apply_item(&item);
            }
        });

        let weak = self.weak();
        page.search.connect_search_changed(move |entry| {
            let Some(g) = weak.upgrade() else { return };
            let text = entry.text().to_string();
            if *g.query.borrow() == text {
                return;
            }
            g.query.replace(text.clone());
            for other in &g.pages {
                if other.search != *entry && other.search.text() != text {
                    other.search.set_text(&text);
                }
            }
            g.refill(None, false);
        });

        let weak = self.weak();
        page.search.connect_activate(move |_| {
            if let Some(g) = weak.upgrade()
                && let Some(item) = g.selected_item()
            {
                g.apply_item(&item);
            }
        });

        // Down from the search box moves into the results.
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak();
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(g) = weak.upgrade() else { return glib::Propagation::Proceed };
            if key == gdk::Key::Down {
                let pos = g.selection.selected();
                g.page().grid.scroll_to(
                    pos,
                    gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                    None,
                );
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        page.search.add_controller(keys);

        // A click on nothing (the backdrop, or the transparent area around the
        // popover) closes the gallery.
        let click = gtk::GestureClick::new();
        click.set_button(1);
        let weak = self.weak();
        let root = page.root.clone();
        click.connect_released(move |gesture, _, x, y| {
            let Some(g) = weak.upgrade() else { return };
            let picked = root.pick(x, y, gtk::PickFlags::DEFAULT);
            let on_nothing = picked.is_none_or(|w| {
                w == root
                    || w.is::<Backdrop>()
                    || w.has_css_class("hg-catcher")
                    || w.has_css_class("hg-content-box")
                    || w.has_css_class("hg-header")
                    || w.has_css_class("hg-footer")
            });
            if on_nothing && gesture.current_button() == 1 {
                g.hide();
            }
        });
        page.root.add_controller(click);
    }

    fn wire_window(self: &Rc<Self>) {
        let weak = self.weak();
        self.selection.connect_selected_item_notify(move |_| {
            if let Some(g) = weak.upgrade() {
                g.settle_focus(true);
            }
        });

        let motion = gtk::EventControllerMotion::new();
        let weak = self.weak();
        motion.connect_motion(move |_, x, y| {
            if let Some(g) = weak.upgrade() {
                g.pointer.set(Some((x, y)));
                g.by_pointer.set(true);
            }
        });
        self.window.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.weak();
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(g) = weak.upgrade() else { return glib::Propagation::Proceed };
            g.by_pointer.set(false);
            g.key_pressed(key, mods)
        });
        let weak = self.weak();
        keys.connect_key_released(move |_, key, _, _| {
            if let Some(g) = weak.upgrade()
                && key == gdk::Key::space
            {
                g.end_peek();
            }
        });
        self.window.add_controller(keys);

        // Files dropped onto the gallery are copied into the album on screen.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = self.weak();
        drop.connect_enter(move |_, _, _| {
            if let Some(g) = weak.upgrade() {
                let target = g.drop_album_name();
                g.drop_label.set_text(&format!("Drop to add to {target}"));
                g.drop_zone.set_reveal_child(true);
            }
            gdk::DragAction::COPY
        });
        let weak = self.weak();
        drop.connect_leave(move |_| {
            if let Some(g) = weak.upgrade() {
                g.drop_zone.set_reveal_child(false);
            }
        });
        let weak = self.weak();
        drop.connect_drop(move |_, value, _, _| {
            let Some(g) = weak.upgrade() else { return false };
            g.drop_zone.set_reveal_child(false);
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            g.import(files);
            true
        });
        self.window.add_controller(drop);

        // Another program taking the keyboard (a lock screen, say) should not
        // leave a hidden gallery holding exclusive focus.
        let weak = self.weak();
        self.window.connect_close_request(move |_| {
            if let Some(g) = weak.upgrade() {
                g.hide();
            }
            glib::Propagation::Stop
        });
    }

    fn key_pressed(self: &Rc<Self>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
        use gdk::Key;
        let page = self.page();
        let in_search = page.search_focused();
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let plain = !ctrl && !mods.contains(gdk::ModifierType::ALT_MASK);
        let stop = glib::Propagation::Stop;

        match key {
            Key::Escape => {
                if self.peeking.get() {
                    self.end_peek();
                } else if !page.search.text().is_empty() {
                    page.search.set_text("");
                    page.grid.grab_focus();
                } else if in_search {
                    page.grid.grab_focus();
                } else {
                    self.hide();
                }
                stop
            }
            Key::Tab | Key::ISO_Left_Tab if !ctrl => {
                self.step_album(
                    if key == Key::ISO_Left_Tab || mods.contains(gdk::ModifierType::SHIFT_MASK) {
                        -1
                    } else {
                        1
                    },
                );
                stop
            }
            Key::f | Key::F if ctrl => {
                page.search.grab_focus();
                stop
            }
            Key::r | Key::R if ctrl => {
                page.rotate.popup();
                stop
            }
            _ if in_search => glib::Propagation::Proceed,
            Key::space if plain => {
                self.start_peek();
                stop
            }
            Key::slash => {
                page.search.grab_focus();
                stop
            }
            Key::f | Key::F if plain => {
                if let Some(item) = self.selected_item() {
                    let changes = self.engine.borrow_mut().toggle_favourite(&item.path);
                    (self.dispatch)(changes);
                }
                stop
            }
            Key::Delete if plain => {
                if let Some(item) = self.selected_item() {
                    self.trash(&item.path);
                }
                stop
            }
            Key::o | Key::O if plain => {
                if let Some(item) = self.selected_item() {
                    self.reveal(&item.path);
                }
                stop
            }
            Key::m | Key::M | Key::Menu if plain => {
                if let Some(item) = self.selected_item()
                    && let Some(tile) =
                        self.bound_tiles(&item.key).into_iter().find(|t| t.is_mapped())
                {
                    self.open_tile_menu(&tile, None);
                }
                stop
            }
            Key::r | Key::R if plain => {
                // A random pick, to look at rather than apply.
                let n = self.store.n_items();
                if n > 1 {
                    let pos = (rotation::seed() % n as u64) as u32;
                    page.grid.scroll_to(
                        pos,
                        gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                        None,
                    );
                }
                stop
            }
            Key::h | Key::j | Key::k | Key::l if plain => {
                let cols = self.columns() as i64;
                let delta = match key {
                    Key::h => -1,
                    Key::l => 1,
                    Key::k => -cols,
                    _ => cols,
                };
                let n = self.store.n_items() as i64;
                if n > 0 {
                    let pos = (self.selection.selected() as i64 + delta).clamp(0, n - 1) as u32;
                    page.grid.scroll_to(
                        pos,
                        gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                        None,
                    );
                }
                stop
            }
            _ if plain => {
                // 1 to 9 jump straight to an album.
                if let Some(digit) =
                    key.to_unicode().and_then(|c| c.to_digit(10)).filter(|d| (1..=9).contains(d))
                {
                    let views = self.engine.borrow().views();
                    if let Some(v) = views.get(digit as usize - 1) {
                        self.set_view(v.id.clone());
                    }
                    return stop;
                }
                glib::Propagation::Proceed
            }
            _ => glib::Propagation::Proceed,
        }
    }

    fn columns(&self) -> u32 {
        let page = self.page();
        let tile_w =
            self.all_bound().iter().map(|(_, t)| t.width()).find(|w| *w > 0).unwrap_or(260) as f64;
        let child_w = tile_w + if self.layout.get() == Layout::Overlay { 18.0 } else { 12.0 };
        ((page.grid.width() as f64 / child_w).floor() as u32).max(1)
    }

    // ----- menus -------------------------------------------------------------

    fn wire_actions(self: &Rc<Self>) {
        let tile_actions = gio::SimpleActionGroup::new();
        let add = |name: &str, param: Option<&glib::VariantTy>, f: ActionFn| {
            let action = gio::SimpleAction::new(name, param);
            let weak = self.weak();
            action.connect_activate(move |_, p| {
                if let Some(g) = weak.upgrade() {
                    f(&g, p);
                }
            });
            tile_actions.add_action(&action);
        };
        add(
            "apply",
            None,
            Box::new(|g, _| {
                if let Some(path) = g.menu_target.borrow().clone()
                    && let Some(pos) = g.position_of(&path)
                    && let Some(item) = g.item_at(pos)
                {
                    g.apply_item(&item);
                }
            }),
        );
        add(
            "favourite",
            None,
            Box::new(|g, _| {
                if let Some(path) = g.menu_target.borrow().clone() {
                    let changes = g.engine.borrow_mut().toggle_favourite(&path);
                    (g.dispatch)(changes);
                }
            }),
        );
        add(
            "reveal",
            None,
            Box::new(|g, _| {
                if let Some(path) = g.menu_target.borrow().clone() {
                    g.reveal(&path);
                }
            }),
        );
        add(
            "trash",
            None,
            Box::new(|g, _| {
                if let Some(path) = g.menu_target.borrow().clone() {
                    g.trash(&path);
                }
            }),
        );
        add(
            "move",
            Some(glib::VariantTy::UINT32),
            Box::new(|g, p| {
                let (Some(path), Some(album)) =
                    (g.menu_target.borrow().clone(), p.and_then(|v| v.get::<u32>()))
                else {
                    return;
                };
                let result = g.engine.borrow_mut().move_to(&path, album as usize);
                match result {
                    Ok(changes) => (g.dispatch)(changes),
                    Err(e) => g.toast(&e, true),
                }
            }),
        );
        self.window.insert_action_group("tile", Some(&tile_actions));

        let gallery_actions = gio::SimpleActionGroup::new();
        let rotate = gio::SimpleAction::new("rotate", Some(glib::VariantTy::UINT64));
        let weak = self.weak();
        rotate.connect_activate(move |_, p| {
            let Some(g) = weak.upgrade() else { return };
            let secs = p.and_then(|v| v.get::<u64>()).unwrap_or(0);
            let rotation = (secs > 0).then(|| {
                Rotation::new(
                    g.view.borrow().clone(),
                    secs,
                    g.shuffle.get(),
                    hyprgallery_core::now(),
                )
            });
            let changes = g.engine.borrow_mut().set_rotation(rotation);
            (g.dispatch)(changes);
        });
        gallery_actions.add_action(&rotate);
        let shuffle = gio::SimpleAction::new_stateful("shuffle", None, &false.to_variant());
        let weak = self.weak();
        shuffle.connect_activate(move |action, _| {
            let Some(g) = weak.upgrade() else { return };
            let on = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&on.to_variant());
            g.shuffle.set(on);
            // Shuffling a rotation that is already running takes effect now.
            let rotation = g.engine.borrow().state.rotation.clone();
            if let Some(mut r) = rotation {
                r.shuffle = on;
                let changes = g.engine.borrow_mut().set_rotation(Some(r));
                (g.dispatch)(changes);
            }
        });
        gallery_actions.add_action(&shuffle);
        self.window.insert_action_group("gallery", Some(&gallery_actions));
        if let Some(r) = &self.engine.borrow().state.rotation {
            self.shuffle.set(r.shuffle);
            shuffle.set_state(&r.shuffle.to_variant());
        }
    }

    fn open_tile_menu(self: &Rc<Self>, tile: &Tile, at: Option<(f64, f64)>) {
        let Some(item) = self.selected_item() else { return };
        self.menu_target.replace(Some(item.path.clone()));
        let (favourite, albums, own_album) = {
            let engine = self.engine.borrow();
            let own = engine.library.wallpapers.get(item.idx).map(|w| w.album);
            let albums: Vec<(usize, String)> =
                engine.library.writable_albums().map(|(i, a)| (i, a.name.clone())).collect();
            (engine.state.is_favourite(&item.path), albums, own)
        };
        let menu = gio::Menu::new();
        let main = gio::Menu::new();
        main.append(Some("Set as wallpaper"), Some("tile.apply"));
        main.append(
            Some(if favourite { "Remove from favourites" } else { "Add to favourites" }),
            Some("tile.favourite"),
        );
        menu.append_section(None, &main);
        let targets: Vec<&(usize, String)> =
            albums.iter().filter(|(i, _)| Some(*i) != own_album).collect();
        if !targets.is_empty() {
            let move_to = gio::Menu::new();
            for (i, name) in targets {
                let entry = gio::MenuItem::new(Some(name), None);
                entry.set_action_and_target_value(
                    Some("tile.move"),
                    Some(&(*i as u32).to_variant()),
                );
                move_to.append_item(&entry);
            }
            let files = gio::Menu::new();
            files.append_submenu(Some("Move to album"), &move_to);
            files.append(Some("Show in folder"), Some("tile.reveal"));
            files.append(Some("Move to trash"), Some("tile.trash"));
            menu.append_section(None, &files);
        } else {
            let files = gio::Menu::new();
            files.append(Some("Show in folder"), Some("tile.reveal"));
            files.append(Some("Move to trash"), Some("tile.trash"));
            menu.append_section(None, &files);
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(tile);
        popover.set_has_arrow(false);
        let (x, y) = at.unwrap_or((tile.width() as f64 / 2.0, tile.height() as f64 / 2.0));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    fn update_rotate(&self) {
        let (rotation, label) = {
            let engine = self.engine.borrow();
            let rotation = engine.state.rotation.clone();
            let label = self.view.borrow().label(&engine.library);
            (rotation, label)
        };
        let menu = gio::Menu::new();
        let every = gio::Menu::new();
        for (secs, text) in [
            (900u64, "Every 15 minutes"),
            (3600, "Every hour"),
            (10_800, "Every 3 hours"),
            (86_400, "Once a day"),
        ] {
            let entry = gio::MenuItem::new(Some(text), None);
            entry.set_action_and_target_value(Some("gallery.rotate"), Some(&secs.to_variant()));
            every.append_item(&entry);
        }
        menu.append_section(Some(&format!("Rotate \u{201C}{label}\u{201D}")), &every);
        let options = gio::Menu::new();
        options.append(Some("Shuffle"), Some("gallery.shuffle"));
        if rotation.is_some() {
            let stop = gio::MenuItem::new(Some("Stop rotating"), None);
            stop.set_action_and_target_value(Some("gallery.rotate"), Some(&0u64.to_variant()));
            options.append_item(&stop);
        }
        menu.append_section(None, &options);
        let text = match &rotation {
            Some(r) => {
                let engine = self.engine.borrow();
                format!(
                    "\u{27F3}  {}  \u{00B7}  {}",
                    r.view.label(&engine.library),
                    rotation::describe_interval(r.every_secs)
                )
            }
            None => "Rotate".to_owned(),
        };
        for page in &self.pages {
            page.rotate.set_menu_model(Some(&menu));
            page.rotate.set_label(&text);
            if rotation.is_some() {
                page.rotate.add_css_class("active");
            } else {
                page.rotate.remove_css_class("active");
            }
        }
    }

    // ----- files -------------------------------------------------------------

    fn drop_album(&self) -> Option<usize> {
        let engine = self.engine.borrow();
        match &*self.view.borrow() {
            ViewId::Album(id) => engine
                .library
                .album_by_id(id)
                .filter(|&i| engine.library.albums[i].origin != Origin::Hyde),
            _ => None,
        }
    }

    fn drop_album_name(&self) -> String {
        let engine = self.engine.borrow();
        match self.drop_album() {
            Some(i) => engine.library.albums[i].name.clone(),
            None => engine
                .config
                .library
                .root_paths()
                .first()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "your library".into()),
        }
    }

    fn import(self: &Rc<Self>, files: Vec<PathBuf>) {
        let album = self.drop_album();
        let result = self.engine.borrow_mut().import(files, album);
        match result {
            Ok(()) => self.toast("Adding\u{2026}", false),
            Err(e) => self.toast(&e, true),
        }
    }

    fn trash(self: &Rc<Self>, path: &Path) {
        if self.engine.borrow().state.current.as_deref() == Some(path) {
            self.toast("That is your current wallpaper. Choose another one first.", true);
            return;
        }
        let file = gio::File::for_path(path);
        let weak = self.weak();
        let path = path.to_owned();
        file.trash_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |result| {
            let Some(g) = weak.upgrade() else { return };
            match result {
                Ok(()) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let changes = g.engine.borrow_mut().forget(&path);
                    (g.dispatch)(changes);
                    g.toast(&format!("Moved {name} to the trash"), false);
                }
                Err(e) => g.toast(&format!("Could not move it to the trash: {e}"), true),
            }
        });
    }

    /// Open the file manager with the file selected, through the standard
    /// FileManager1 interface, or just the folder when nothing answers it.
    fn reveal(self: &Rc<Self>, path: &Path) {
        let uri = gio::File::for_path(path).uri().to_string();
        let folder = path.parent().map(|p| gio::File::for_path(p).uri().to_string());
        let args = (vec![uri], String::new()).to_variant();
        let weak = self.weak();
        gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |bus| {
            let fallback = move || {
                if let Some(folder) = folder {
                    let _ = gio::AppInfo::launch_default_for_uri(
                        &folder,
                        None::<&gio::AppLaunchContext>,
                    );
                }
            };
            let Ok(bus) = bus else { return fallback() };
            bus.call(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1",
                "ShowItems",
                Some(&args),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                None::<&gio::Cancellable>,
                move |result| {
                    if result.is_err() {
                        fallback();
                    }
                },
            );
        });
        if let Some(g) = weak.upgrade() {
            g.hide();
        }
    }

    fn toast(self: &Rc<Self>, message: &str, error: bool) {
        if !self.shown.get() {
            return;
        }
        self.toast_label.set_text(message);
        if error {
            self.toast_label.add_css_class("error");
        } else {
            self.toast_label.remove_css_class("error");
        }
        self.toast.set_reveal_child(true);
        let weak = self.weak();
        self.toast_timer.run(if error { 4200 } else { 2600 }, move || {
            if let Some(g) = weak.upgrade() {
                g.toast.set_reveal_child(false);
            }
        });
    }

    // ----- changes from elsewhere --------------------------------------------

    pub fn on_changes(self: &Rc<Self>, changes: &[Change]) {
        let mut rebuild = false;
        let mut refill = false;
        for change in changes {
            match change {
                Change::Library => {
                    rebuild = true;
                    refill = true;
                }
                Change::Meta(key) => {
                    let items: Vec<(Tile, Item)> = self
                        .bound_tiles(key)
                        .into_iter()
                        .filter_map(|t| {
                            let engine = self.engine.borrow();
                            let idx =
                                engine.library.wallpapers.iter().position(|w| w.key == *key)?;
                            let w = &engine.library.wallpapers[idx];
                            Some((t, Item { idx, key: key.clone(), path: w.path.clone() }))
                        })
                        .collect();
                    for (tile, item) in items {
                        tile.set_data(&self.tile_data(&item));
                        self.load_thumb(&tile, &item);
                    }
                    if self.selected_item().is_some_and(|i| i.key == *key) {
                        self.settle_focus(false);
                    }
                }
                Change::Favourite(path) => {
                    let fav = self.engine.borrow().state.is_favourite(path);
                    for (_, tile) in self.all_bound() {
                        if self.tile_path(&tile).as_deref() == Some(path.as_path()) {
                            tile.set_favourite(fav);
                        }
                    }
                    rebuild = true;
                    if *self.view.borrow() == ViewId::Favourites {
                        refill = true;
                    }
                    self.toast(
                        if fav { "Added to favourites" } else { "Removed from favourites" },
                        false,
                    );
                }
                Change::Current | Change::Applying => {
                    let (current, applying): (Option<PathBuf>, Vec<(String, bool)>) = {
                        let engine = self.engine.borrow();
                        let current = engine.state.current.clone();
                        let applying = self
                            .all_bound()
                            .iter()
                            .map(|(k, _)| {
                                let path = engine
                                    .library
                                    .wallpapers
                                    .iter()
                                    .find(|w| w.key == *k)
                                    .map(|w| w.path.clone());
                                (k.clone(), path.is_some_and(|p| engine.is_applying(&p)))
                            })
                            .collect();
                        (current, applying)
                    };
                    for (key, tile) in self.all_bound() {
                        let path = self.tile_path(&tile);
                        tile.set_current(path.is_some() && path == current);
                        let busy = applying.iter().any(|(k, b)| *k == key && *b);
                        tile.set_applying(busy);
                    }
                }
                Change::Rotation => self.update_rotate(),
                Change::Config => self.reconfigure(),
                Change::Indexing => {}
                Change::Error(message) => self.toast(message, true),
                Change::Notice(message) => self.toast(message, false),
            }
        }
        if rebuild {
            self.rebuild_albums();
        }
        if refill {
            let keep = self.selected_item().map(|i| i.path);
            self.refill(keep, false);
        }
        self.update_status();
    }

    fn tile_path(&self, tile: &Tile) -> Option<PathBuf> {
        let key = tile.key();
        let engine = self.engine.borrow();
        engine.library.wallpapers.iter().find(|w| w.key == key).map(|w| w.path.clone())
    }

    /// Render the window into a PNG. For screenshots in the docs and for
    /// checking the layout on a machine without a screen to look at.
    pub fn save_png(&self, path: &Path) -> anyhow::Result<()> {
        let (w, h) = (self.window.width(), self.window.height());
        let paintable = gtk::WidgetPaintable::new(Some(&self.window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, w as f64, h as f64);
        let node = snapshot.to_node().ok_or_else(|| anyhow::anyhow!("nothing was drawn"))?;
        let renderer = self
            .window
            .native()
            .and_then(|n| n.renderer())
            .ok_or_else(|| anyhow::anyhow!("window has no renderer"))?;
        let texture = renderer
            .render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32)));
        texture.save_to_png(path)?;
        Ok(())
    }

    /// Select by position, for scripted snapshots.
    pub fn select_index(&self, pos: u32) {
        self.selection.set_selected(pos);
        self.page().grid.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
    }

    pub fn thumbnails_pending(&self) -> bool {
        self.all_bound().iter().any(|(_, t)| !t.has_thumb())
    }
}

fn glyph(id: &ViewId) -> Option<&'static str> {
    match id {
        ViewId::Favourites => Some("\u{2665}"),
        ViewId::Recent => Some("\u{25F7}"),
        ViewId::Colour => Some("\u{25D1}"),
        ViewId::Animated => Some("\u{25B6}"),
        ViewId::Album(_) => None,
    }
}

fn scroll_into_view(scroll: &gtk::ScrolledWindow, widget: &gtk::Widget) {
    let Some(bounds) = widget.compute_bounds(scroll) else { return };
    let adj = scroll.hadjustment();
    let (x, w) = (bounds.x() as f64 + adj.value(), bounds.width() as f64);
    let page = adj.page_size();
    if x < adj.value() {
        adj.set_value(x - 24.0);
    } else if x + w > adj.value() + page {
        adj.set_value(x + w - page + 24.0);
    }
}
