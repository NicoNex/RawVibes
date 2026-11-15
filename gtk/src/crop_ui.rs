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
    /// Zoomed in (only the level line is drawn; the crop is edited fitted).
    pub zoomed: bool,
}

pub struct CropUi {
    pub section: gtk::Box,
    pub overlay: gtk::DrawingArea,
    pub edit: gtk::ToggleButton,
    pub ratio: gtk::DropDown,
    pub swap: gtk::Button,
    pub angle: Slider,
    /// Straighten along a line dragged over the photo.
    pub level: gtk::ToggleButton,
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
        let level = gtk::ToggleButton::builder()
            .icon_name("object-rotate-right-symbolic")
            .tooltip_text(gettext("Straighten along a line: drag over a horizon or an edge"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        level.update_property(&[gtk::accessible::Property::Label(&gettext("Straighten along a line"))]);
        let e = edit.clone();
        level.connect_toggled(move |b| {
            if b.is_active() {
                e.set_active(true);
            }
        });
        if let Some(head) = angle.root.first_child().and_downcast::<gtk::Box>() {
            head.insert_child_after(&level, head.first_child().as_ref());
        }

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
        // The line being dragged with the level tool, in overlay pixels.
        let line: Rc<Cell<Option<[f64; 4]>>> = Rc::new(Cell::new(None));
        let o = overlay.clone();
        level.connect_toggled(move |b| o.set_cursor_from_name(b.is_active().then_some("crosshair")));
        let (sh, ln) = (shown.clone(), line.clone());
        overlay.set_draw_func(move |_, cr, w, h| {
            if let Some([x0, y0, x1, y1]) = ln.get() {
                cr.set_source_rgba(1.0, 0.85, 0.2, 0.95);
                cr.set_line_width(2.0);
                cr.move_to(x0, y0);
                cr.line_to(x1, y1);
                let _ = cr.stroke();
            }
            let sh = sh.borrow();
            if sh.frame[0] <= 0.0 || sh.zoomed {
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
        let (g, sh, o, lv, ln) = (grab.clone(), shown.clone(), overlay.clone(), level.clone(), line.clone());
        drag.connect_drag_begin(move |d, x, y| {
            g.set(None);
            if lv.is_active() {
                ln.set(Some([x, y, x, y]));
            } else {
                let sh = sh.borrow();
                if sh.zoomed {
                    return; // the drag pans the view
                }
                let (ox, oy, sx, sy) = placement(o.width() as f64, o.height() as f64, sh.frame);
                let p = [((x - ox) / sx) as f32, ((y - oy) / sy) as f32];
                let tol = [(GRAB / sx) as f32, (GRAB / sy) as f32];
                g.set(crop::hit(&sh.crop, p, tol).map(|h| (h, sh.crop, sx, sy)));
                if g.get().is_none() {
                    return;
                }
            }
            // Ours: the canvas must not pan under a line or a handle.
            d.set_state(gtk::EventSequenceState::Claimed);
        });
        let (sh, s) = (shown.clone(), sender.clone());
        let (ln, o) = (line.clone(), overlay.clone());
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some([x0, y0, ..]) = ln.get() {
                ln.set(Some([x0, y0, x0 + dx, y0 + dy]));
                return o.queue_draw();
            }
            let Some((handle, start, sx, sy)) = grab.get() else { return };
            let c = {
                let sh = sh.borrow();
                let [w, h] = sh.frame.map(|v| v as u32);
                crop::drag(start, handle, [(dx / sx) as f32, (dy / sy) as f32], sh.aspect, w, h)
            };
            s.input(Msg::CropRect(c.rect));
        });
        let (lv, adj, o) = (level.clone(), angle.adj.clone(), overlay.clone());
        drag.connect_drag_end(move |_, dx, dy| {
            let Some(_) = line.take() else { return };
            lv.set_active(false);
            o.queue_draw();
            if dx.hypot(dy) >= 10.0 {
                adj.set_value(adj.value() + level_turn(dx, dy));
            }
        });
        overlay.add_controller(drag);

        Self { section, overlay, edit, ratio, swap, angle, level, shown }
    }
}

/// Degrees to add to the straighten angle so a line drawn on the shown frame
/// (screen y down) becomes level, or plumb when it is nearer vertical.
fn level_turn(dx: f64, dy: f64) -> f64 {
    let t = dy.atan2(dx).to_degrees();
    t - (t / 90.0).round() * 90.0
}

#[cfg(test)]
mod tests {
    use super::level_turn;

    #[test]
    fn level_turn_picks_the_nearest_axis() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(level_turn(100.0, 0.0), 0.0));
        assert!(near(level_turn(-100.0, 0.0), 0.0));
        assert!(near(level_turn(100.0, 100.0 * 5f64.to_radians().tan()), 5.0));
        // Drawn right to left: same line, same turn.
        assert!(near(level_turn(-100.0, -100.0 * 5f64.to_radians().tan()), 5.0));
        // Nearly vertical, leaning 5° off: turned to plumb.
        assert!(near(level_turn(100.0 * 5f64.to_radians().tan(), 100.0), -5.0));
    }
}
