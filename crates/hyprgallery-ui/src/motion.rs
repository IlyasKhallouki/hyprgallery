//! Animation, driven by the frame clock.
//!
//! Everything that moves goes through [`Animated`], which samples the widget's
//! frame clock rather than a timer. Frames land exactly on vblank, so motion
//! stays smooth on 120Hz and variable-refresh displays where a 16ms timer
//! visibly judders. Starting a new animation from wherever the last one had
//! got to is what lets hover and focus effects reverse mid-flight without a
//! jump.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_out_quint(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(5)
}

pub fn ease_in_out_cubic(t: f64) -> f64 {
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

pub fn ease_in_cubic(t: f64) -> f64 {
    t * t * t
}

pub type Ease = fn(f64) -> f64;

/// A number that moves smoothly to wherever it is told.
pub struct Animated {
    value: Cell<f64>,
    target: Cell<f64>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
}

impl Animated {
    pub fn new(value: f64) -> Rc<Self> {
        Rc::new(Self {
            value: Cell::new(value),
            target: Cell::new(value),
            tick: RefCell::new(None),
        })
    }

    pub fn get(&self) -> f64 {
        self.value.get()
    }

    pub fn target(&self) -> f64 {
        self.target.get()
    }

    fn cancel(&self) {
        if let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
    }

    /// Jump straight to `value`.
    pub fn set(&self, value: f64) {
        self.cancel();
        self.value.set(value);
        self.target.set(value);
    }

    /// Move to `to` over `ms`, calling `frame` on every frame with the new
    /// value and `done` once it arrives.
    ///
    /// A widget that is not on screen has no frame clock ticking, so there the
    /// value jumps and both callbacks run at once.
    pub fn animate(
        self: &Rc<Self>,
        widget: &impl IsA<gtk::Widget>,
        to: f64,
        ms: u32,
        ease: Ease,
        frame: impl Fn(f64) + 'static,
        done: impl FnOnce() + 'static,
    ) {
        self.cancel();
        self.target.set(to);
        let from = self.value.get();
        if !widget.is_mapped() || ms == 0 || (from - to).abs() < 1e-4 {
            self.value.set(to);
            frame(to);
            done();
            return;
        }
        let start = Cell::new(None::<i64>);
        let duration = ms as f64 * 1000.0;
        let done = RefCell::new(Some(Box::new(done) as Box<dyn FnOnce()>));
        let this = Rc::downgrade(self);
        let id = widget.add_tick_callback(move |_, clock| {
            let Some(this) = this.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let t0 = *start.get().get_or_insert(now);
            start.set(Some(t0));
            let t = ((now - t0) as f64 / duration).clamp(0.0, 1.0);
            let value = from + (to - from) * ease(t);
            this.value.set(value);
            frame(value);
            if t >= 1.0 {
                // The id is spent once this returns Break. Dropping it rather
                // than removing it is what avoids a double free warning.
                drop(this.tick.borrow_mut().take());
                if let Some(done) = done.borrow_mut().take() {
                    done();
                }
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        *self.tick.borrow_mut() = Some(id);
    }
}

/// Run `f` once after `ms`, cancelling any earlier call through the same
/// handle. For debouncing work that follows the pointer or the keyboard.
#[derive(Default)]
pub struct Debounce {
    source: RefCell<Option<glib::SourceId>>,
}

impl Debounce {
    pub fn run(self: &Rc<Self>, ms: u64, f: impl FnOnce() + 'static) {
        self.cancel();
        let this = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            if let Some(this) = this.upgrade() {
                this.source.borrow_mut().take();
            }
            f();
        });
        *self.source.borrow_mut() = Some(id);
    }

    pub fn cancel(&self) {
        if let Some(id) = self.source.borrow_mut().take() {
            id.remove();
        }
    }
}

mod motion_imp {
    use super::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct Motion {
        pub progress: Cell<f64>,
        /// Scale at progress 0.
        pub from_scale: Cell<f64>,
        /// Vertical offset at progress 0, in pixels.
        pub from_dy: Cell<f64>,
        /// Where scaling is anchored, as fractions of the size.
        pub origin: Cell<(f64, f64)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Motion {
        const NAME: &'static str = "HgMotion";
        type Type = super::Motion;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("hg-motion");
        }
    }

    impl ObjectImpl for Motion {
        fn constructed(&self) {
            self.parent_constructed();
            self.progress.set(1.0);
            self.from_scale.set(1.0);
            self.origin.set((0.5, 0.5));
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Motion {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let Some(child) = obj.first_child() else { return };
            let p = self.progress.get();
            if p >= 0.999 {
                obj.snapshot_child(&child, snapshot);
                return;
            }
            if p <= 0.001 {
                return;
            }
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let (ox, oy) = self.origin.get();
            let scale = (self.from_scale.get() + (1.0 - self.from_scale.get()) * p) as f32;
            let dy = (self.from_dy.get() * (1.0 - p)) as f32;
            snapshot.push_opacity(p);
            snapshot.save();
            let pivot = gtk::graphene::Point::new(w * ox as f32, h * oy as f32 + dy);
            snapshot.translate(&pivot);
            snapshot.scale(scale, scale);
            snapshot.translate(&gtk::graphene::Point::new(-pivot.x(), -pivot.y() + dy));
            obj.snapshot_child(&child, snapshot);
            snapshot.restore();
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    /// A container that fades, scales and slides its one child by a progress
    /// value, without touching layout. Used for opening and closing.
    pub struct Motion(ObjectSubclass<motion_imp::Motion>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Motion {
    pub fn new(
        child: &impl IsA<gtk::Widget>,
        from_scale: f64,
        from_dy: f64,
        origin: (f64, f64),
    ) -> Self {
        use gtk::subclass::prelude::*;
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.from_scale.set(from_scale);
        imp.from_dy.set(from_dy);
        imp.origin.set(origin);
        child.set_parent(&obj);
        obj
    }

    pub fn set_progress(&self, p: f64) {
        use gtk::subclass::prelude::*;
        self.imp().progress.set(p);
        self.set_can_target(p > 0.5);
        self.queue_draw();
    }

    pub fn set_origin(&self, origin: (f64, f64)) {
        use gtk::subclass::prelude::*;
        self.imp().origin.set(origin);
    }

    pub fn origin(&self) -> (f64, f64) {
        use gtk::subclass::prelude::*;
        self.imp().origin.get()
    }

    /// Change how progress 0 looks: the starting scale, vertical offset and
    /// the point scaling is anchored to.
    pub fn configure(&self, from_scale: f64, from_dy: f64, origin: (f64, f64)) {
        use gtk::subclass::prelude::*;
        let imp = self.imp();
        imp.from_scale.set(from_scale);
        imp.from_dy.set(from_dy);
        imp.origin.set(origin);
    }
}
