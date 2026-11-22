//! The Lens section's profile row: which lensfun profile corrects the photo, a search to
//! pick another, and the focal length and aperture a manual lens does not record.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use relm4::adw::prelude::*;
use relm4::{ComponentSender, adw, gtk};

use crate::{App, Msg, gettext};

/// What the profile row says.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Found from the EXIF.
    Auto(String),
    /// Picked by hand.
    Chosen(String),
    /// A profile, but no focal length to read it at (a manual lens): type it in.
    NeedsFocal(String),
    NotFound,
    /// "No profile" picked.
    Off,
    /// No lensfun database installed.
    NoDatabase,
}

/// The row's subtitle for `status`.
pub fn profile_label(status: &Status) -> String {
    match status {
        Status::Auto(name) => gettext!("{} · automatic", name),
        Status::Chosen(name) => gettext!("{} · chosen", name),
        Status::NeedsFocal(name) => gettext!("{}: type in the focal length", name),
        Status::NotFound => gettext("None found"),
        Status::Off => gettext("No profile"),
        Status::NoDatabase => gettext("No lens database: install lensfun"),
    }
}

/// The profile row, its search popover, and the focal length and aperture rows.
pub struct ProfileUi {
    pub list: gtk::ListBox,
    row: adw::ActionRow,
    focal: adw::SpinRow,
    aperture: adw::SpinRow,
    results: gtk::ListBox,
    found: Rc<RefCell<Vec<(String, String)>>>,
    syncing: Rc<Cell<bool>>,
}

impl ProfileUi {
    pub fn new(sender: &ComponentSender<App>) -> Self {
        let syncing = Rc::new(Cell::new(false));
        let row = adw::ActionRow::builder().title(gettext("Profile")).subtitle("—").css_classes(["property"]).build();
        let search = gtk::SearchEntry::builder().placeholder_text(gettext("Search lenses")).build();
        let snd = sender.clone();
        search.connect_search_changed(move |e| snd.input(Msg::LensSearch(e.text().into())));
        // Automatic and No profile first, then what the search finds.
        let fixed =
            gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        for (title, pick) in [
            (gettext("Automatic (from the photo)"), None),
            (gettext("No profile"), Some((String::new(), String::new()))),
        ] {
            let r = adw::ActionRow::builder().title(title).activatable(true).build();
            let snd = sender.clone();
            r.connect_activated(move |_| snd.input(Msg::LensPick(pick.clone())));
            fixed.append(&r);
        }
        let results =
            gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        let found: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
        let (snd, f) = (sender.clone(), found.clone());
        results.connect_row_activated(move |_, r| {
            if let Some(pick) = f.borrow().get(r.index() as usize).cloned() {
                snd.input(Msg::LensPick(Some(pick)));
            }
        });
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&results)
            .max_content_height(320)
            .propagate_natural_height(true)
            .build();
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).width_request(320).build();
        body.append(&search);
        body.append(&fixed);
        body.append(&scroll);
        let popover = gtk::Popover::builder().child(&body).build();
        // Picking closes it.
        let p = popover.clone();
        fixed.connect_row_activated(move |_, _| p.popdown());
        let p = popover.clone();
        results.connect_row_activated(move |_, _| p.popdown());
        let pick = gtk::MenuButton::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text(gettext("Choose the lens profile"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .popover(&popover)
            .build();
        row.add_suffix(&pick);
        let focal = adw::SpinRow::with_range(0.0, 2000.0, 1.0);
        focal.set_title(&gettext("Focal length (mm)"));
        let aperture = adw::SpinRow::with_range(0.0, 64.0, 0.1);
        aperture.set_title(&gettext("Aperture (f/)"));
        aperture.set_digits(1);
        for s in [&focal, &aperture] {
            let (snd, sync, f, a) = (sender.clone(), syncing.clone(), focal.clone(), aperture.clone());
            s.connect_value_notify(move |_| {
                if !sync.get() {
                    snd.input(Msg::LensManual(f.value() as f32, a.value() as f32));
                }
            });
        }
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        list.append(&row);
        list.append(&focal);
        list.append(&aperture);
        Self { list, row, focal, aperture, results, found, syncing }
    }

    /// Shows `status`; the focal length and aperture rows only when they are `asked` for.
    pub fn show(&self, status: &Status, focal: f32, aperture: f32, asked: bool) {
        self.row.set_subtitle(&profile_label(status));
        self.syncing.set(true);
        self.focal.set_value(focal as f64);
        self.aperture.set_value(aperture as f64);
        self.syncing.set(false);
        self.focal.set_visible(asked);
        self.aperture.set_visible(asked);
    }

    /// The search's results, as (maker, model).
    pub fn results(&self, lenses: Vec<(String, String)>) {
        self.results.remove_all();
        for (maker, model) in &lenses {
            self.results.append(
                &adw::ActionRow::builder().title(model.as_str()).subtitle(maker.as_str()).activatable(true).build(),
            );
        }
        *self.found.borrow_mut() = lenses;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every status reads differently and names the lens when there is one (in any language:
    /// another test switches the process to Italian).
    #[test]
    fn labels_say_where_the_profile_comes_from() {
        let name = "Canon EF 16-35mm f/2.8L USM".to_string();
        let all = [
            Status::Auto(name.clone()),
            Status::Chosen(name.clone()),
            Status::NeedsFocal(name.clone()),
            Status::NotFound,
            Status::Off,
            Status::NoDatabase,
        ];
        let labels: Vec<String> = all.iter().map(profile_label).collect();
        assert!(labels[..3].iter().all(|l| l.contains(&name)), "{labels:?}");
        assert!(labels.iter().enumerate().all(|(i, a)| labels[i + 1..].iter().all(|b| a != b)), "{labels:?}");
        assert!(labels[5].contains("lensfun"));
    }
}
