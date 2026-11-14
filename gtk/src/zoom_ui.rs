//! Zoom controls and gestures of the Grade canvas. Points and deltas are
//! sent in device pixels of the picture area (the canvas minus its margin).

use std::cell::Cell;
use std::rc::Rc;

use gettextrs::gettext;
use relm4::gtk::prelude::*;
use relm4::{ComponentSender, gtk};

use crate::{App, CANVAS_MARGIN, Msg};

#[derive(Clone, Copy, Debug)]
pub enum ZoomCmd {
    Fit,
    Actual,
    In,
    Out,
    /// Wheel/touchpad scroll of `dy` steps about a point.
    Scroll {
        dy: f64,
        at: [f32; 2],
    },
    /// Drag by `d` with mouse `button` (1 primary, 2 middle).
    Pan {
        d: [f32; 2],
        button: u32,
    },
    /// Double-click: fit ↔ 100% about a point.
    Toggle([f32; 2]),
}

pub struct ZoomUi {
    pub tools: gtk::Box,
    pub label: gtk::Label,
}

impl ZoomUi {
    pub fn new(canvas: &gtk::Overlay, sender: &ComponentSender<App>) -> Self {
        let to_area = {
            let c = canvas.clone();
            move |x: f64, y: f64| {
                let sf = c.scale_factor() as f64;
                [((x - CANVAS_MARGIN) * sf) as f32, ((y - CANVAS_MARGIN) * sf) as f32]
            }
        };

        // Canvas size, through a probe that takes no input.
        let probe = gtk::DrawingArea::builder().can_target(false).build();
        let (s, c) = (sender.clone(), canvas.clone());
        probe.connect_resize(move |_, w, h| {
            let sf = c.scale_factor() as f64;
            let area = [w, h].map(|v| (((v as f64 - 2.0 * CANVAS_MARGIN) * sf) as f32).max(1.0));
            s.input(Msg::CanvasSize(area));
        });
        canvas.add_overlay(&probe);

        let pointer = Rc::new(Cell::new([0.0f32; 2]));
        let motion = gtk::EventControllerMotion::new();
        let (p, f) = (pointer.clone(), to_area.clone());
        motion.connect_motion(move |_, x, y| p.set(f(x, y)));
        canvas.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let (s, p) = (sender.clone(), pointer.clone());
        scroll.connect_scroll(move |_, _, dy| {
            s.input(Msg::Zoom(ZoomCmd::Scroll { dy, at: p.get() }));
            gtk::glib::Propagation::Stop
        });
        canvas.add_controller(scroll);

        let click = gtk::GestureClick::new();
        let (s, f) = (sender.clone(), to_area.clone());
        click.connect_pressed(move |_, n, x, y| {
            if n == 2 {
                s.input(Msg::Zoom(ZoomCmd::Toggle(f(x, y))));
            }
        });
        canvas.add_controller(click);

        let drag = gtk::GestureDrag::builder().button(0).build();
        let last = Rc::new(Cell::new((0.0, 0.0)));
        let l = last.clone();
        drag.connect_drag_begin(move |_, _, _| l.set((0.0, 0.0)));
        let (s, c) = (sender.clone(), canvas.clone());
        drag.connect_drag_update(move |g, dx, dy| {
            let (lx, ly) = last.replace((dx, dy));
            let sf = c.scale_factor() as f64;
            let d = [((dx - lx) * sf) as f32, ((dy - ly) * sf) as f32];
            s.input(Msg::Zoom(ZoomCmd::Pan { d, button: g.current_button() }));
        });
        canvas.add_controller(drag);

        let tools = gtk::Box::builder()
            .css_classes(["linked", "osd", "canvas-tools"])
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        let label = gtk::Label::builder().width_chars(5).css_classes(["numeric", "zoom-label"]).build();
        let button = |icon: &str, tip: String, cmd: ZoomCmd| {
            let b = gtk::Button::builder().icon_name(icon).tooltip_text(&tip).build();
            b.update_property(&[gtk::accessible::Property::Label(&tip)]);
            let s = sender.clone();
            b.connect_clicked(move |_| s.input(Msg::Zoom(cmd)));
            b
        };
        tools.append(&button("zoom-out-symbolic", gettext("Zoom out (Ctrl+−)"), ZoomCmd::Out));
        tools.append(&label);
        tools.append(&button("zoom-in-symbolic", gettext("Zoom in (Ctrl++)"), ZoomCmd::In));
        tools.append(&button("zoom-fit-best-symbolic", gettext("Fit (Ctrl+0)"), ZoomCmd::Fit));
        tools.append(&button("zoom-original-symbolic", gettext("Actual size (Ctrl+1)"), ZoomCmd::Actual));
        canvas.add_overlay(&tools);
        Self { tools, label }
    }
}
