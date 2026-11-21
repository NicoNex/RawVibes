//! Filmstrip under the Grade canvas: the photos graded together, each shown
//! through the current grade, and a grid view of all of them, so a look can
//! be judged on skin, sky, foliage and night shots at once.

use crate::gettext;
use rawvibes_core::sidecar::Label;
use relm4::adw::prelude::*;
use relm4::gtk::gdk;
use relm4::{ComponentSender, gtk};

use crate::{App, Msg, vbox};

/// Most photos the strip holds: each one is re-rendered on every grade change.
pub const MAX_ROLL: usize = 12;
/// Longest side of strip photos, kept decoded so a profile change only re-encodes them.
pub const ROLL_PX: u32 = 720;

struct Card {
    root: gtk::Overlay,
    button: gtk::Button,
    pic: gtk::Picture,
    grid_pic: gtk::Picture,
    grid_button: gtk::Button,
    /// Stars and colour label, on the strip card and the grid cell.
    marks: [gtk::Label; 2],
}

pub struct Filmstrip {
    /// Revealed once a photo is open.
    pub root: gtk::Revealer,
    /// The grid view, shown in place of the single photo.
    pub grid_root: gtk::ScrolledWindow,
    strip: gtk::Box,
    add: gtk::Button,
    /// Shows only the photos with at least some stars, or with a colour label.
    filter: gtk::DropDown,
    grid: gtk::FlowBox,
    cards: Vec<Card>,
}

/// Stars as text: "★★★☆☆", empty when unrated.
pub fn stars(rating: u8) -> String {
    if rating == 0 { String::new() } else { (1..=5).map(|i| if i <= rating { '★' } else { '☆' }).collect() }
}

/// CSS class of a label: its colour (see the app CSS).
pub fn label_class(l: Label) -> &'static str {
    match l {
        Label::None => "label-none",
        Label::Red => "label-red",
        Label::Yellow => "label-yellow",
        Label::Green => "label-green",
        Label::Blue => "label-blue",
        Label::Purple => "label-purple",
    }
}

/// Gives `w` the class of label `l`, and drops the others.
pub fn set_label_class(w: &impl IsA<gtk::Widget>, l: Label) {
    Label::ALL.into_iter().for_each(|x| w.remove_css_class(label_class(x)));
    w.add_css_class(label_class(l));
}

pub fn label_name(l: Label) -> String {
    match l {
        Label::None => gettext("No label"),
        Label::Red => gettext("Red"),
        Label::Yellow => gettext("Yellow"),
        Label::Green => gettext("Green"),
        Label::Blue => gettext("Blue"),
        Label::Purple => gettext("Purple"),
    }
}

/// Filter row `f` of the dropdown: 0 all, 1–5 at least that many stars, then the colour labels.
fn shown(f: u32, rating: u8, label: Label) -> bool {
    match f {
        0 => true,
        1..=5 => rating as u32 >= f,
        _ => Label::ALL.get(f as usize - 5) == Some(&label),
    }
}

impl Filmstrip {
    pub fn new(sender: &ComponentSender<App>) -> Self {
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(gettext("Add photos to grade together"))
            .valign(gtk::Align::Center)
            .css_classes(["circular", "roll-add"])
            .margin_start(6)
            .margin_end(6)
            .build();
        add.update_property(&[gtk::accessible::Property::Label(&gettext("Add photos"))]);
        let s = sender.clone();
        add.connect_clicked(move |_| s.input(Msg::Open));
        let mut items = vec![gettext("All photos")];
        items.extend((1..=5).map(|n| if n == 5 { stars(5) } else { format!("{} +", stars(n).trim_end_matches('☆')) }));
        items.extend(Label::ALL[1..].iter().map(|&l| label_name(l)));
        let items: Vec<&str> = items.iter().map(String::as_str).collect();
        let filter = gtk::DropDown::builder()
            .model(&gtk::StringList::new(&items))
            .tooltip_text(gettext("Show only some photos: by stars or colour label"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        filter.update_property(&[gtk::accessible::Property::Label(&gettext("Filter photos"))]);
        let s = sender.clone();
        filter.connect_selected_notify(move |_| s.input(Msg::RollFilter));
        let strip =
            gtk::Box::builder().spacing(8).margin_start(12).margin_end(12).margin_top(8).margin_bottom(8).build();
        strip.append(&add);
        strip.append(&filter);
        let scroll = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&strip)
            .css_classes(["filmstrip"])
            .build();
        let root = gtk::Revealer::builder()
            .child(&scroll)
            .reveal_child(false)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .build();

        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            // Pictures shrink to their cell, so the columns come from these bounds.
            .min_children_per_line(2)
            .max_children_per_line(3)
            .row_spacing(12)
            .column_spacing(12)
            .margin_start(24)
            .margin_end(24)
            .margin_top(60) // clear of the canvas tools
            .margin_bottom(24)
            .valign(gtk::Align::Start)
            .build();
        let grid_root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .css_classes(["canvas"])
            .build();
        Self { root, grid_root, strip, add, filter, grid, cards: Vec::new() }
    }

    /// Recreates the cards for photos named `names` (structure changed).
    pub fn rebuild(&mut self, names: &[String], active: Option<usize>, sender: &ComponentSender<App>) {
        for c in self.cards.drain(..) {
            self.strip.remove(&c.root);
        }
        self.grid.remove_all();
        for (i, name) in names.iter().enumerate() {
            let pic = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Cover)
                .width_request(132)
                .height_request(88)
                .build();
            let label = gtk::Label::builder()
                .label(name)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .max_width_chars(16)
                .css_classes(["caption"])
                .build();
            let mark = mark_label();
            let body = vbox(4);
            body.append(&pic);
            body.append(&label);
            body.append(&mark);
            let button =
                gtk::Button::builder().child(&body).css_classes(["flat", "roll-card"]).tooltip_text(name).build();
            button.update_property(&[gtk::accessible::Property::Label(name)]);
            let s = sender.clone();
            button.connect_clicked(move |_| s.input(Msg::RollSelect(i)));
            // The remove button sits beside the card button, never inside it.
            let remove = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .tooltip_text(gettext("Remove from the filmstrip"))
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .margin_top(4)
                .margin_end(4)
                .css_classes(["circular", "osd", "roll-remove"])
                .build();
            remove.update_property(&[gtk::accessible::Property::Label(&gettext!("Remove {}", name))]);
            let s = sender.clone();
            remove.connect_clicked(move |_| s.input(Msg::RollRemove(i)));
            let card = gtk::Overlay::builder().child(&button).css_classes(["roll-item"]).build();
            card.add_overlay(&remove);
            self.strip.insert_child_after(&card, self.cards.last().map(|c| &c.root));

            let grid_pic = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Contain)
                .height_request(240)
                .hexpand(true)
                .can_shrink(true)
                .build();
            let caption = gtk::Label::builder()
                .label(name)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .css_classes(["caption", "dim-label"])
                .build();
            let grid_mark = mark_label();
            let cell = vbox(6);
            cell.append(&grid_pic);
            cell.append(&caption);
            cell.append(&grid_mark);
            let grid_button = gtk::Button::builder()
                .child(&cell)
                .css_classes(["flat", "grid-card"])
                .tooltip_text(gettext("Edit this photo"))
                .build();
            grid_button.update_property(&[gtk::accessible::Property::Label(name)]);
            let s = sender.clone();
            grid_button.connect_clicked(move |_| {
                s.input(Msg::RollSelect(i));
                s.input(Msg::Grid(false));
            });
            self.grid.append(&grid_button);
            self.cards.push(Card { root: card, button, pic, grid_pic, grid_button, marks: [mark, grid_mark] });
        }
        // The add button and the filter always come last.
        self.strip.reorder_child_after(&self.add, self.cards.last().map(|c| &c.root));
        self.strip.reorder_child_after(&self.filter, Some(&self.add));
        self.set_active(active);
        self.root.set_reveal_child(!names.is_empty());
    }

    pub fn set_active(&self, active: Option<usize>) {
        for (i, c) in self.cards.iter().enumerate() {
            for b in [&c.button, &c.grid_button] {
                if Some(i) == active { b.add_css_class("active") } else { b.remove_css_class("active") }
            }
        }
    }

    /// Whether the filter lets a photo with these marks through.
    pub fn shows(&self, rating: u8, label: Label) -> bool {
        shown(self.filter.selected(), rating, label)
    }

    /// Each photo's stars and label, and only those the filter lets through.
    pub fn set_marks(&self, marks: &[(u8, Label)]) {
        let f = self.filter.selected();
        for (c, &(rating, label)) in self.cards.iter().zip(marks) {
            for m in &c.marks {
                // A label alone shows as a dot in its colour.
                m.set_label(&if rating == 0 { "●".to_string() } else { stars(rating) });
                set_label_class(m, label);
                m.set_visible(rating > 0 || label != Label::None);
            }
            let visible = shown(f, rating, label);
            c.root.set_visible(visible);
            if let Some(cell) = c.grid_button.parent() {
                cell.set_visible(visible);
            }
        }
    }

    /// The graded rendition of photo `i`.
    pub fn set_frame(&self, i: usize, texture: &gdk::MemoryTexture) {
        if let Some(c) = self.cards.get(i) {
            c.pic.set_paintable(Some(texture));
            c.grid_pic.set_paintable(Some(texture));
        }
    }
}

/// Stars line of a card; its colour label is a dot drawn by CSS.
fn mark_label() -> gtk::Label {
    gtk::Label::builder().css_classes(["caption", "roll-mark"]).visible(false).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_rows() {
        assert!(shown(0, 0, Label::None));
        assert!(shown(3, 4, Label::None) && !shown(3, 2, Label::Red));
        assert!(shown(6, 0, Label::Red) && !shown(6, 5, Label::Green));
        assert!(shown(10, 0, Label::Purple));
        assert_eq!(stars(0), "");
        assert_eq!(stars(2), "★★☆☆☆");
    }
}
