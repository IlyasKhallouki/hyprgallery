//! An album cover for the popover's shelf: its first few wallpapers fanned
//! out like a stack of prints.

use crate::backdrop::cover;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};

const W: i32 = 136;
const H: i32 = 86;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Cover {
        pub thumbs: RefCell<Vec<gdk::Texture>>,
        pub glyph: RefCell<Option<String>>,
        pub tint: Cell<Option<gdk::RGBA>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Cover {
        const NAME: &'static str = "HgCover";
        type Type = super::Cover;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("hg-cover");
        }
    }

    impl ObjectImpl for Cover {}

    impl WidgetImpl for Cover {
        fn measure(&self, orientation: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let size = if orientation == gtk::Orientation::Horizontal { W } else { H };
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            // Each card is inset so the rotated ones stay inside the widget.
            let (cw, ch) = (w * 0.86, h * 0.86);
            let thumbs = self.thumbs.borrow();
            let angles = [-7.0f32, 5.0, 0.0];
            let n = thumbs.len().clamp(1, 3);
            for (i, angle) in angles.iter().enumerate().skip(3 - n) {
                let thumb = thumbs.get(i - (3 - n));
                snapshot.save();
                snapshot.translate(&graphene::Point::new(w / 2.0, h / 2.0));
                snapshot.rotate(*angle);
                snapshot.translate(&graphene::Point::new(-cw / 2.0, -ch / 2.0));
                let rect = graphene::Rect::new(0.0, 0.0, cw, ch);
                let rounded = gsk::RoundedRect::from_rect(rect, 9.0);
                snapshot.append_outset_shadow(
                    &rounded,
                    &gdk::RGBA::new(0.0, 0.0, 0.0, 0.35),
                    0.0,
                    3.0,
                    0.0,
                    8.0,
                );
                snapshot.push_rounded_clip(&rounded);
                match (thumb, self.tint.get()) {
                    (Some(t), _) => cover(snapshot, t.upcast_ref(), cw, ch),
                    (None, Some(tint)) => snapshot.append_color(&tint, &rect),
                    (None, None) => {
                        snapshot.append_color(&gdk::RGBA::new(1.0, 1.0, 1.0, 0.08), &rect)
                    }
                }
                snapshot.pop();
                snapshot.restore();
            }
            if let Some(glyph) = self.glyph.borrow().as_deref() {
                let layout = obj.create_pango_layout(Some(glyph));
                let attrs = pango::AttrList::new();
                attrs.insert(pango::AttrFloat::new_scale(2.0));
                layout.set_attributes(Some(&attrs));
                let (tw, th) = layout.pixel_size();
                snapshot.save();
                snapshot
                    .translate(&graphene::Point::new((w - tw as f32) / 2.0, (h - th as f32) / 2.0));
                snapshot.append_layout(&layout, &gdk::RGBA::new(1.0, 1.0, 1.0, 0.92));
                snapshot.restore();
            }
        }
    }
}

glib::wrapper! {
    pub struct Cover(ObjectSubclass<imp::Cover>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Cover {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Cover {
    pub fn set_thumbs(&self, thumbs: Vec<gdk::Texture>) {
        self.imp().thumbs.replace(thumbs);
        self.queue_draw();
    }

    /// For smart albums: a symbol over a solid card, used when there are no
    /// thumbnails to show (an empty Favourites, say) and on top of them.
    pub fn set_glyph(&self, glyph: Option<&str>, tint: Option<gdk::RGBA>) {
        self.imp().glyph.replace(glyph.map(str::to_owned));
        self.imp().tint.set(tint);
        self.queue_draw();
    }
}
