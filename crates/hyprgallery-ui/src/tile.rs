//! One wallpaper in the grid.
//!
//! Drawn by hand rather than assembled from widgets. A tile is a picture, a
//! name, a few badges and a spinner, all of which fade or zoom; as widgets that
//! would be six of them per tile with their own CSS transitions, and a grid of
//! a few hundred. Drawing it keeps every effect on the frame clock and keeps
//! scrolling cheap. Its box (corners, outline, shadow) is still plain CSS.

use crate::backdrop::cover;
use crate::motion::{self, Animated};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

mod imp {
    use super::*;

    pub struct Tile {
        pub thumb: RefCell<Option<gdk::Paintable>>,
        /// A playing GIF or video while focused, drawn instead of the thumb.
        pub preview: RefCell<Option<(gdk::Paintable, glib::SignalHandlerId)>>,
        pub colour: Cell<gdk::RGBA>,
        pub name: RefCell<String>,
        pub key: RefCell<String>,
        pub favourite: Cell<bool>,
        pub current: Cell<bool>,
        pub failed: Cell<bool>,
        pub badge: Cell<Option<&'static str>>,
        pub applying: Cell<bool>,
        pub spin: RefCell<Option<gtk::TickCallbackId>>,
        pub spin_phase: Cell<f64>,
        pub hover: Rc<Animated>,
        pub selected: Rc<Animated>,
        pub loaded: Rc<Animated>,
        pub min_width: Cell<i32>,
    }

    impl Default for Tile {
        fn default() -> Self {
            Self {
                thumb: RefCell::new(None),
                preview: RefCell::new(None),
                colour: Cell::new(gdk::RGBA::new(0.2, 0.2, 0.24, 1.0)),
                name: RefCell::new(String::new()),
                key: RefCell::new(String::new()),
                favourite: Cell::new(false),
                current: Cell::new(false),
                failed: Cell::new(false),
                badge: Cell::new(None),
                applying: Cell::new(false),
                spin: RefCell::new(None),
                spin_phase: Cell::new(0.0),
                hover: Animated::new(0.0),
                selected: Animated::new(0.0),
                loaded: Animated::new(0.0),
                min_width: Cell::new(250),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tile {
        const NAME: &'static str = "HgTile";
        type Type = super::Tile;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("hg-tile");
        }
    }

    impl ObjectImpl for Tile {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            let motion = gtk::EventControllerMotion::new();
            let weak = obj.downgrade();
            motion.connect_enter(move |_, _, _| {
                if let Some(tile) = weak.upgrade() {
                    tile.hovered(true);
                }
            });
            let weak = obj.downgrade();
            motion.connect_leave(move |_| {
                if let Some(tile) = weak.upgrade() {
                    tile.hovered(false);
                }
            });
            obj.add_controller(motion);
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            if let Some(id) = self.spin.borrow_mut().take() {
                id.remove();
            }
            if let Some((p, h)) = self.preview.borrow_mut().take() {
                p.disconnect(h);
            }
        }
    }

    impl WidgetImpl for Tile {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let min = self.min_width.get();
            match orientation {
                gtk::Orientation::Horizontal => (min, min * 5 / 4, -1, -1),
                _ => {
                    // Thumbnails are cut to 16:9, so tiles are too.
                    let w = if for_size > 0 { for_size } else { min * 5 / 4 };
                    let h = w * 9 / 16;
                    (h, h, -1, -1)
                }
            }
        }

        fn size_allocate(&self, _width: i32, _height: i32, _baseline: i32) {
            // The right-click menu is parented to the tile. A popover only
            // appears if its parent presents it during allocation, which stock
            // widgets do and a hand-drawn one has to do itself.
            let mut child = self.obj().first_child();
            while let Some(c) = child {
                if let Some(popover) = c.downcast_ref::<gtk::Popover>() {
                    popover.present();
                }
                child = c.next_sibling();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.draw(snapshot);
        }
    }

    impl Tile {
        fn draw(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let bounds = graphene::Rect::new(0.0, 0.0, w, h);
            snapshot.append_color(&self.colour.get(), &bounds);

            let hover = self.hover.get();
            let selected = self.selected.get();
            let lift = hover.max(selected);

            // A media stream has no size until its first frame is decoded, or
            // ever if no decoder could open it. Keep the thumbnail until then.
            let preview = self
                .preview
                .borrow()
                .as_ref()
                .map(|(p, _)| p.clone())
                .filter(|p| p.intrinsic_width() > 0);
            let picture = preview.or_else(|| self.thumb.borrow().clone());
            if let Some(p) = picture {
                let zoom = 1.0 + 0.05 * lift as f32;
                snapshot.push_opacity(self.loaded.get());
                snapshot.save();
                snapshot.translate(&graphene::Point::new(w / 2.0, h / 2.0));
                snapshot.scale(zoom, zoom);
                snapshot.translate(&graphene::Point::new(-w / 2.0, -h / 2.0));
                cover(snapshot, &p, w, h);
                snapshot.restore();
                snapshot.pop();
            } else if self.failed.get() {
                let layout = obj.create_pango_layout(Some("Cannot read this file"));
                text(snapshot, &layout, 0.85, pango::Weight::Normal, w - 24.0);
                let (tw, th) = layout.pixel_size();
                draw_text(snapshot, &layout, (w - tw as f32) / 2.0, (h - th as f32) / 2.0, 0.6);
            }

            // Name, over a gradient, only while hovered or focused. A grid of
            // labels is noise; the one you are looking at is information.
            if lift > 0.01 {
                let a = lift as f32;
                snapshot.append_linear_gradient(
                    &graphene::Rect::new(0.0, h * 0.45, w, h * 0.55),
                    &graphene::Point::new(0.0, h * 0.45),
                    &graphene::Point::new(0.0, h),
                    &[
                        gsk::ColorStop::new(0.0, gdk::RGBA::new(0.0, 0.0, 0.0, 0.0)),
                        gsk::ColorStop::new(1.0, gdk::RGBA::new(0.0, 0.0, 0.0, 0.7 * a)),
                    ],
                );
                let layout = obj.create_pango_layout(Some(&self.name.borrow()));
                text(snapshot, &layout, 0.95, pango::Weight::Semibold, w - 24.0);
                let (_, th) = layout.pixel_size();
                let slide = 6.0 * (1.0 - a);
                draw_text(snapshot, &layout, 12.0, h - 10.0 - th as f32 + slide, a);
            }

            let mut x = 9.0;
            if self.current.get() {
                x += pill(
                    snapshot,
                    &obj,
                    x,
                    9.0,
                    "Current",
                    gdk::RGBA::new(1.0, 1.0, 1.0, 0.92),
                    gdk::RGBA::new(0.06, 0.06, 0.08, 1.0),
                ) + 6.0;
            }
            if let Some(badge) = self.badge.get() {
                pill(
                    snapshot,
                    &obj,
                    x,
                    9.0,
                    badge,
                    gdk::RGBA::new(0.0, 0.0, 0.0, 0.55),
                    gdk::RGBA::new(1.0, 1.0, 1.0, 0.95),
                );
            }
            if self.favourite.get() {
                let layout = obj.create_pango_layout(Some("\u{2665}"));
                text(snapshot, &layout, 1.15, pango::Weight::Normal, 40.0);
                let (tw, _) = layout.pixel_size();
                let x = w - 11.0 - tw as f32;
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x, 7.0));
                snapshot.append_layout(&layout, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.45));
                snapshot.translate(&graphene::Point::new(0.0, -1.0));
                snapshot.append_layout(&layout, &gdk::RGBA::new(1.0, 0.42, 0.52, 1.0));
                snapshot.restore();
            }

            if self.applying.get() {
                snapshot.append_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.35), &bounds);
                let cr = snapshot.append_cairo(&bounds);
                let r = (h * 0.11).clamp(10.0, 22.0) as f64;
                let start = self.spin_phase.get();
                cr.set_line_width(3.0);
                cr.set_line_cap(gtk::cairo::LineCap::Round);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.25);
                cr.arc(w as f64 / 2.0, h as f64 / 2.0, r, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                cr.arc(w as f64 / 2.0, h as f64 / 2.0, r, start, start + 1.6);
                let _ = cr.stroke();
            }
        }
    }

    fn text(
        _snapshot: &gtk::Snapshot,
        layout: &pango::Layout,
        scale: f64,
        weight: pango::Weight,
        max_w: f32,
    ) {
        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrFloat::new_scale(scale));
        attrs.insert(pango::AttrInt::new_weight(weight));
        layout.set_attributes(Some(&attrs));
        layout.set_width((max_w.max(1.0) * pango::SCALE as f32) as i32);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        layout.set_single_paragraph_mode(true);
    }

    fn draw_text(snapshot: &gtk::Snapshot, layout: &pango::Layout, x: f32, y: f32, alpha: f32) {
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x, y + 1.0));
        snapshot.append_layout(layout, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.5 * alpha));
        snapshot.translate(&graphene::Point::new(0.0, -1.0));
        snapshot.append_layout(layout, &gdk::RGBA::new(1.0, 1.0, 1.0, alpha));
        snapshot.restore();
    }

    /// A small rounded label. Returns its width.
    fn pill(
        snapshot: &gtk::Snapshot,
        obj: &super::Tile,
        x: f32,
        y: f32,
        label: &str,
        bg: gdk::RGBA,
        fg: gdk::RGBA,
    ) -> f32 {
        let layout = obj.create_pango_layout(Some(label));
        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrFloat::new_scale(0.72));
        attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        attrs.insert(pango::AttrInt::new_letter_spacing(pango::SCALE / 2));
        layout.set_attributes(Some(&attrs));
        let (tw, th) = layout.pixel_size();
        let (pw, ph) = (tw as f32 + 14.0, th as f32 + 6.0);
        let rect = graphene::Rect::new(x, y, pw, ph);
        snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, ph / 2.0));
        snapshot.append_color(&bg, &rect);
        snapshot.pop();
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x + 7.0, y + 3.0));
        snapshot.append_layout(&layout, &fg);
        snapshot.restore();
        pw
    }
}

glib::wrapper! {
    pub struct Tile(ObjectSubclass<imp::Tile>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Everything a tile shows, gathered by whoever binds it.
#[derive(Debug, Clone, Default)]
pub struct TileData {
    pub key: String,
    pub name: String,
    pub colour: Option<gdk::RGBA>,
    pub favourite: bool,
    pub current: bool,
    pub applying: bool,
    pub failed: bool,
    pub badge: Option<&'static str>,
}

impl Tile {
    pub fn new(min_width: i32) -> Self {
        let tile: Self = glib::Object::new();
        tile.imp().min_width.set(min_width);
        tile
    }

    pub fn key(&self) -> String {
        self.imp().key.borrow().clone()
    }

    /// Show a different wallpaper. Clears the picture if the file changed.
    pub fn set_data(&self, data: &TileData) {
        let imp = self.imp();
        if *imp.key.borrow() != data.key {
            imp.key.replace(data.key.clone());
            imp.thumb.replace(None);
            imp.loaded.set(0.0);
            self.stop_preview();
        }
        imp.name.replace(data.name.clone());
        if let Some(c) = data.colour {
            imp.colour.set(c);
        }
        imp.favourite.set(data.favourite);
        imp.current.set(data.current);
        imp.failed.set(data.failed);
        imp.badge.set(data.badge);
        self.set_applying(data.applying);
        self.update_property(&[gtk::accessible::Property::Label(&data.name)]);
        self.queue_draw();
    }

    pub fn set_favourite(&self, favourite: bool) {
        self.imp().favourite.set(favourite);
        self.queue_draw();
    }

    pub fn set_current(&self, current: bool) {
        self.imp().current.set(current);
        self.queue_draw();
    }

    pub fn has_thumb(&self) -> bool {
        self.imp().thumb.borrow().is_some()
    }

    /// Put the thumbnail in, fading if it arrived after the tile was drawn.
    pub fn set_thumb(&self, texture: gdk::Texture, fade: bool) {
        let imp = self.imp();
        imp.thumb.replace(Some(texture.upcast()));
        if fade {
            imp.loaded.set(0.0);
            let weak = self.downgrade();
            imp.loaded.animate(
                self,
                1.0,
                240,
                motion::ease_out_cubic,
                move |_| {
                    if let Some(t) = weak.upgrade() {
                        t.queue_draw();
                    }
                },
                || {},
            );
        } else {
            imp.loaded.set(1.0);
            self.queue_draw();
        }
    }

    pub fn set_selected(&self, selected: bool) {
        let weak = self.downgrade();
        self.imp().selected.animate(
            self,
            if selected { 1.0 } else { 0.0 },
            if selected { 160 } else { 240 },
            motion::ease_out_cubic,
            move |_| {
                if let Some(t) = weak.upgrade() {
                    t.queue_draw();
                }
            },
            || {},
        );
        if !selected {
            self.stop_preview();
        }
    }

    /// For a tile being bound: show its state without animating into it.
    pub fn set_selected_now(&self, selected: bool) {
        self.imp().selected.set(if selected { 1.0 } else { 0.0 });
        self.queue_draw();
    }

    fn hovered(&self, hovered: bool) {
        let weak = self.downgrade();
        self.imp().hover.animate(
            self,
            if hovered { 1.0 } else { 0.0 },
            if hovered { 200 } else { 320 },
            motion::ease_out_cubic,
            move |_| {
                if let Some(t) = weak.upgrade() {
                    t.queue_draw();
                }
            },
            || {},
        );
    }

    pub fn set_applying(&self, applying: bool) {
        let imp = self.imp();
        if imp.applying.replace(applying) == applying {
            return;
        }
        if let Some(id) = imp.spin.borrow_mut().take() {
            id.remove();
        }
        if applying {
            let id = self.add_tick_callback(|tile, clock| {
                let t = clock.frame_time() as f64 / 1_000_000.0 / motion::time_scale();
                tile.imp().spin_phase.set((t * 5.0) % std::f64::consts::TAU);
                tile.queue_draw();
                glib::ControlFlow::Continue
            });
            *imp.spin.borrow_mut() = Some(id);
        }
        self.queue_draw();
    }

    /// Play an animated wallpaper inside the tile.
    pub fn play_preview(&self, paintable: gdk::Paintable) {
        self.stop_preview();
        let weak = self.downgrade();
        let handler = paintable.connect_invalidate_contents(move |_| {
            if let Some(t) = weak.upgrade() {
                t.queue_draw();
            }
        });
        self.imp().preview.replace(Some((paintable, handler)));
        self.imp().loaded.set(1.0);
    }

    pub fn stop_preview(&self) {
        if let Some((p, h)) = self.imp().preview.borrow_mut().take() {
            p.disconnect(h);
            if let Some(media) = p.downcast_ref::<gtk::MediaStream>() {
                media.pause();
            }
            self.queue_draw();
        }
    }
}
