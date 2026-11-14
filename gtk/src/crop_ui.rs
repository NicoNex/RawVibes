//! Crop section of the sidebar and the crop overlay drawn on the Grade canvas.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gettextrs::gettext;
use rawvalley_core::crop::{self, Crop, Handle, PRESETS};
use relm4::gtk::prelude::*;
use relm4::{ComponentSender, gtk};

use crate::widgets::Slider;
use crate::{App, CANVAS_MARGIN, Msg, section};

/// A drag in progress: the handle, the crop at its start, and the
/// frame's logical pixels per normalised unit.
type Grab = (Handle, Crop, f64, f64);

/// Grab distance of the edges and corners, in logical pixels.
const GRAB: f64 = 12.0;

/// What the overlay draws: the crop over the whole straightened frame.
#[derive(Default)]
pub struct Shown {
    pub crop: Crop,
    /// Full-resolution size of the oriented image (0 = nothing shown).
    pub frame: [f32; 2],
    /// Pixel aspect resizing keeps (None: free).
    pub aspect: Option<f32>,
}

pub struct CropUi {
    pub section: gtk::Box,
    pub overlay: gtk::DrawingArea,
    pub edit: gtk::ToggleButton,
    pub ratio: gtk::DropDown,
    pub swap: gtk::Button,
    pub angle: Slider,
    pub shown: Rc<RefCell<Shown>>,
}

/// The frame's placement in the overlay: origin and logical pixels per
/// normalised unit, as `ContentFit::Contain` inside the picture margin.
fn placement(w: f64, h: f64, frame: [f32; 2]) -> (f64, f64, f64, f64) {
    let (fw, fh) = (frame[0] as f64, frame[1] as f64);
    let (aw, ah) = (w - 2.0 * CANVAS_MARGIN, h - 2.0 * CANVAS_MARGIN);
    let s = (aw / fw).min(ah / fh);
    (CANVAS_MARGIN + (aw - fw * s) / 2.0, CANVAS_MARGIN + (ah - fh * s) / 2.0, fw * s, fh * s)
}

impl CropUi {
    pub fn new(sender: &ComponentSender<App>) -> Self {
        let shown = Rc::new(RefCell::new(Shown::default()));

        let edit = gtk::ToggleButton::builder()
            .label(gettext("Edit crop"))
            .tooltip_text(gettext("Edit crop (R)"))
            .css_classes(["pill"])
            .hexpand(true)
            .build();
        let s = sender.clone();
        edit.connect_toggled(move |b| s.input(Msg::CropEdit(b.is_active())));

        let labels: Vec<String> = PRESETS
            .iter()
            .map(|(l, _)| match *l {
                "Free" => gettext("Free"),
                "Original" => gettext("Original"),
                _ => (*l).to_string(),
            })
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let ratio = gtk::DropDown::from_strings(&labels);
        ratio.set_hexpand(true);
        ratio.update_property(&[gtk::accessible::Property::Label(&gettext("Aspect ratio"))]);
        let s = sender.clone();
        ratio.connect_selected_notify(move |d| s.input(Msg::CropRatio(d.selected() as usize)));
        let swap = gtk::Button::builder()
            .icon_name("object-flip-vertical-symbolic")
            .tooltip_text(gettext("Swap orientation"))
            .sensitive(false)
            .build();
        swap.update_property(&[gtk::accessible::Property::Label(&gettext("Swap orientation"))]);
        let s = sender.clone();
        swap.connect_clicked(move |_| s.input(Msg::CropSwap));
        let ratio_row = gtk::Box::builder().spacing(6).css_classes(["slider-row"]).build();
        ratio_row.append(&ratio);
        ratio_row.append(&swap);

        let angle = Slider::new(&gettext("Straighten"), -45.0, 45.0, 0.0, 1, None);
        let s = sender.clone();
        angle.connect(move |v| s.input(Msg::CropAngle(v as f32)));

        let reset = gtk::Button::builder().label(gettext("Reset")).css_classes(["flat"]).build();
        let s = sender.clone();
        reset.connect_clicked(move |_| s.input(Msg::CropReset));
        let buttons = gtk::Box::builder().spacing(6).css_classes(["slider-row"]).build();
        buttons.append(&edit);
        buttons.append(&reset);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.append(&ratio_row);
        body.append(&angle.root);
        body.append(&buttons);
        let section = section(&gettext("Crop"), &body);

        let overlay = gtk::DrawingArea::builder().visible(false).build();
        let sh = shown.clone();
        overlay.set_draw_func(move |_, cr, w, h| {
            let sh = sh.borrow();
            if sh.frame[0] <= 0.0 {
                return;
            }
            let (ox, oy, sx, sy) = placement(w as f64, h as f64, sh.frame);
            let [x, y, cw, ch] = sh.crop.rect.map(|v| v as f64);
            let (rx, ry, rw, rh) = (ox + x * sx, oy + y * sy, cw * sx, ch * sy);
            // Outside the crop, dimmed.
            cr.set_fill_rule(gtk::cairo::FillRule::EvenOdd);
            cr.rectangle(ox, oy, sx, sy);
            cr.rectangle(rx, ry, rw, rh);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
            let _ = cr.fill();
            // Rule of thirds.
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
            cr.set_line_width(1.0);
            for i in [1.0, 2.0] {
                cr.move_to(rx + rw * i / 3.0, ry);
                cr.line_to(rx + rw * i / 3.0, ry + rh);
                cr.move_to(rx, ry + rh * i / 3.0);
                cr.line_to(rx + rw, ry + rh * i / 3.0);
            }
            let _ = cr.stroke();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            cr.set_line_width(1.5);
            cr.rectangle(rx, ry, rw, rh);
            let _ = cr.stroke();
            // Handles on the corners and edge midpoints.
            for (hx, hy) in [0.0, 0.5, 1.0].iter().flat_map(|&u| [0.0, 0.5, 1.0].map(move |v| (u, v))) {
                if (hx, hy) != (0.5, 0.5) {
                    cr.rectangle(rx + rw * hx - 4.0, ry + rh * hy - 4.0, 8.0, 8.0);
                }
            }
            let _ = cr.fill();
        });

        let drag = gtk::GestureDrag::new();
        let grab: Rc<Cell<Option<Grab>>> = Rc::new(Cell::new(None));
        let (g, sh, o) = (grab.clone(), shown.clone(), overlay.clone());
        drag.connect_drag_begin(move |_, x, y| {
            let sh = sh.borrow();
            let (ox, oy, sx, sy) = placement(o.width() as f64, o.height() as f64, sh.frame);
            let p = [((x - ox) / sx) as f32, ((y - oy) / sy) as f32];
            let tol = [(GRAB / sx) as f32, (GRAB / sy) as f32];
            g.set(crop::hit(&sh.crop, p, tol).map(|h| (h, sh.crop, sx, sy)));
        });
        let (sh, s) = (shown.clone(), sender.clone());
        drag.connect_drag_update(move |_, dx, dy| {
            let Some((handle, start, sx, sy)) = grab.get() else { return };
            let c = {
                let sh = sh.borrow();
                let [w, h] = sh.frame.map(|v| v as u32);
                crop::drag(start, handle, [(dx / sx) as f32, (dy / sy) as f32], sh.aspect, w, h)
            };
            s.input(Msg::CropRect(c.rect));
        });
        overlay.add_controller(drag);

        Self { section, overlay, edit, ratio, swap, angle, shown }
    }
}
