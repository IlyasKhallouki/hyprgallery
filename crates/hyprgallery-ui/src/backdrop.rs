//! The full-screen backdrop behind the overlay.
//!
//! It shows a soft wash of whichever wallpaper is focused and crossfades as
//! focus moves, so browsing previews the mood of each wallpaper without
//! applying anything. Peeking fades in the real image, sharp, at screen size.

use crate::motion::{self, Animated};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;

mod imp {
    use super::*;

    pub struct Backdrop {
        pub from: RefCell<Option<gdk::Paintable>>,
        pub to: RefCell<Option<gdk::Paintable>>,
        pub mix: Rc<Animated>,
        pub peek: RefCell<Option<gdk::Paintable>>,
        pub peek_amount: Rc<Animated>,
        pub peek_handler: RefCell<Option<(gdk::Paintable, glib::SignalHandlerId)>>,
    }

    impl Default for Backdrop {
        fn default() -> Self {
            Self {
                from: RefCell::new(None),
                to: RefCell::new(None),
                mix: Animated::new(1.0),
                peek: RefCell::new(None),
                peek_amount: Animated::new(0.0),
                peek_handler: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Backdrop {
        const NAME: &'static str = "HgBackdrop";
        type Type = super::Backdrop;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("hg-backdrop");
        }
    }

    impl ObjectImpl for Backdrop {}

    impl WidgetImpl for Backdrop {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let bounds = graphene::Rect::new(0.0, 0.0, w, h);
            snapshot.append_color(&gdk::RGBA::new(0.03, 0.03, 0.05, 1.0), &bounds);

            let mix = self.mix.get();
            if let Some(from) = self.from.borrow().as_ref()
                && mix < 0.999
            {
                cover(snapshot, from, w, h);
            }
            if let Some(to) = self.to.borrow().as_ref() {
                snapshot.push_opacity(mix);
                cover(snapshot, to, w, h);
                snapshot.pop();
            }

            let peek = self.peek_amount.get();
            // Darken towards the edges and the bottom, where the grid and the
            // hints sit, so white text reads on any wallpaper.
            let scrim = (1.0 - peek) as f32;
            if scrim > 0.0 {
                let black = |a: f32| gdk::RGBA::new(0.0, 0.0, 0.0, a * scrim);
                snapshot.append_linear_gradient(
                    &bounds,
                    &graphene::Point::new(0.0, 0.0),
                    &graphene::Point::new(0.0, h),
                    &[
                        gsk::ColorStop::new(0.0, black(0.42)),
                        gsk::ColorStop::new(0.35, black(0.30)),
                        gsk::ColorStop::new(1.0, black(0.62)),
                    ],
                );
                let radius = w.max(h) * 0.75;
                snapshot.append_radial_gradient(
                    &bounds,
                    &graphene::Point::new(w / 2.0, h / 2.0),
                    radius,
                    radius,
                    0.0,
                    1.0,
                    &[gsk::ColorStop::new(0.55, black(0.0)), gsk::ColorStop::new(1.0, black(0.38))],
                );
            }

            if peek > 0.001
                && let Some(p) = self.peek.borrow().as_ref()
            {
                snapshot.push_opacity(peek);
                cover(snapshot, p, w, h);
                snapshot.pop();
            }
        }
    }
}

/// Draw `paintable` scaled to cover `w` x `h`, centred, the way a wallpaper
/// program fills the screen.
pub fn cover(snapshot: &gtk::Snapshot, paintable: &gdk::Paintable, w: f32, h: f32) {
    let (iw, ih) = (paintable.intrinsic_width() as f32, paintable.intrinsic_height() as f32);
    let (iw, ih) = if iw > 0.0 && ih > 0.0 { (iw, ih) } else { (w, h) };
    let scale = (w / iw).max(h / ih);
    let (dw, dh) = (iw * scale, ih * scale);
    snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, w, h));
    snapshot.save();
    snapshot.translate(&graphene::Point::new((w - dw) / 2.0, (h - dh) / 2.0));
    match paintable.downcast_ref::<gdk::Texture>() {
        // Linear filtering explicitly: the backdrop is a small image stretched
        // across the screen, and nearest-neighbour would show its pixels.
        Some(texture) => snapshot.append_scaled_texture(
            texture,
            gsk::ScalingFilter::Linear,
            &graphene::Rect::new(0.0, 0.0, dw, dh),
        ),
        None => paintable.snapshot(snapshot, dw as f64, dh as f64),
    }
    snapshot.restore();
    snapshot.pop();
}

glib::wrapper! {
    pub struct Backdrop(ObjectSubclass<imp::Backdrop>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Backdrop {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Backdrop {
    /// Show `paintable`, crossfading from whatever was there.
    pub fn show(&self, paintable: Option<gdk::Paintable>, animate: bool) {
        let imp = self.imp();
        if imp.to.borrow().as_ref() == paintable.as_ref() {
            return;
        }
        // Mid-crossfade, freeze the blend so far as the new starting point,
        // rather than snapping back to the older image.
        let previous = imp.to.replace(paintable);
        if imp.mix.get() > 0.5 || imp.from.borrow().is_none() {
            imp.from.replace(previous);
        }
        if !animate {
            imp.mix.set(1.0);
            self.queue_draw();
            return;
        }
        imp.mix.set(0.0);
        let this = self.downgrade();
        imp.mix.animate(
            self,
            1.0,
            420,
            motion::ease_in_out_cubic,
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.queue_draw();
                }
            },
            || {},
        );
    }

    /// Replace the peek image without changing whether peek is showing.
    pub fn set_peek_paintable(&self, paintable: Option<gdk::Paintable>) {
        let imp = self.imp();
        if let Some((old, handler)) = imp.peek_handler.borrow_mut().take() {
            old.disconnect(handler);
        }
        // Animated paintables (video, GIF) announce new frames this way.
        if let Some(p) = &paintable
            && !p.flags().contains(gdk::PaintableFlags::STATIC_CONTENTS)
        {
            let this = self.downgrade();
            let handler = p.connect_invalidate_contents(move |_| {
                if let Some(this) = this.upgrade() {
                    this.queue_draw();
                }
            });
            *imp.peek_handler.borrow_mut() = Some((p.clone(), handler));
        }
        imp.peek.replace(paintable);
        self.queue_draw();
    }

    pub fn set_peeking(&self, peeking: bool) {
        let this = self.downgrade();
        self.imp().peek_amount.animate(
            self,
            if peeking { 1.0 } else { 0.0 },
            if peeking { 220 } else { 260 },
            motion::ease_out_cubic,
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.queue_draw();
                }
            },
            || {},
        );
    }

    pub fn is_peeking(&self) -> bool {
        self.imp().peek_amount.target() > 0.5
    }
}
