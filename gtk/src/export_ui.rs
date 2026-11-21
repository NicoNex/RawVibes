//! Export Photo dialog: format, JPEG quality with a live size estimate,
//! then a file chooser; progress and Stop while the worker exports.

use gettextrs::gettext;
use rawvibes_core::export::{JPEG_QUALITY, PhotoFormat};
use relm4::adw::prelude::*;
use relm4::{ComponentSender, adw, gtk};

use crate::{App, Msg, settings, vbox};

pub struct ExportUi {
    pub dialog: adw::Dialog,
    format: adw::ComboRow,
    quality: adw::SpinRow,
    size: adw::ActionRow,
    estimate: adw::ActionRow,
    bar: gtk::ProgressBar,
    go: gtk::Button,
}

pub fn format_label(f: PhotoFormat) -> String {
    match f {
        PhotoFormat::Jpeg => gettext("JPEG · 8-bit, for sharing"),
        PhotoFormat::Png16 => gettext("PNG · 16-bit, lossless"),
        PhotoFormat::Tiff16 => gettext("TIFF · 16-bit, for print and editing"),
    }
}

/// The last format and JPEG quality used.
pub fn remembered() -> (PhotoFormat, u8) {
    let format = settings::value(settings::EXPORT_FORMAT).and_then(|t| PhotoFormat::from_tag(&t));
    let quality = settings::value(settings::JPEG_QUALITY).and_then(|q| q.parse().ok());
    (format.unwrap_or(PhotoFormat::Jpeg), quality.unwrap_or(JPEG_QUALITY).clamp(1, 100))
}

pub fn build(sender: &ComponentSender<App>) -> ExportUi {
    let (format, quality) = remembered();
    let names: Vec<String> = PhotoFormat::ALL.into_iter().map(format_label).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let format_row = adw::ComboRow::builder().title(gettext("Format")).model(&gtk::StringList::new(&names)).build();
    let snd = sender.clone();
    format_row.connect_selected_notify(move |_| snd.input(Msg::ExportSettings));
    let quality_row = adw::SpinRow::builder()
        .title(gettext("Quality"))
        .adjustment(&gtk::Adjustment::new(quality as f64, 1.0, 100.0, 1.0, 10.0, 0.0))
        .build();
    let snd = sender.clone();
    quality_row.connect_value_notify(move |_| snd.input(Msg::ExportSettings));
    let size = adw::ActionRow::builder().title(gettext("Size")).css_classes(["property"]).build();
    let estimate = adw::ActionRow::builder().title(gettext("File size")).css_classes(["property"]).build();
    let group = adw::PreferencesGroup::builder()
        .description(gettext("Full resolution of the crop, with every edit, in sRGB."))
        .build();
    for row in
        [format_row.upcast_ref::<gtk::Widget>(), quality_row.upcast_ref(), size.upcast_ref(), estimate.upcast_ref()]
    {
        group.add(row);
    }

    let bar = gtk::ProgressBar::builder().visible(false).show_text(true).build();
    let go = gtk::Button::builder()
        .label(gettext("Export…"))
        .css_classes(["suggested-action", "pill"])
        .halign(gtk::Align::Center)
        .build();
    let snd = sender.clone();
    go.connect_clicked(move |_| snd.input(Msg::ExportGo));

    let body = vbox(18);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.append(&group);
    body.append(&bar);
    body.append(&go);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));
    let dialog = adw::Dialog::builder()
        .title(gettext("Export Photo"))
        .content_width(440)
        .child(&toolbar)
        .default_widget(&go)
        .build();
    let snd = sender.clone();
    dialog.connect_closed(move |_| snd.input(Msg::ExportClosed));
    let ui = ExportUi { dialog, format: format_row, quality: quality_row, size, estimate, bar, go };
    ui.set_format(format);
    ui
}

impl ExportUi {
    pub fn format(&self) -> PhotoFormat {
        selected(&self.format)
    }

    pub fn quality(&self) -> u8 {
        self.quality.value().round() as u8
    }

    /// Shows `f`'s controls (quality is JPEG only) and selects it.
    pub fn set_format(&self, f: PhotoFormat) {
        let i = PhotoFormat::ALL.iter().position(|&x| x == f).unwrap_or(0) as u32;
        self.format.set_selected(i);
        self.quality.set_visible(f == PhotoFormat::Jpeg);
    }

    pub fn set_size(&self, w: u32, h: u32) {
        self.size.set_subtitle(&gettext!("{} × {} px", w, h));
    }

    /// `bytes`, or `None` while it is being estimated.
    pub fn set_estimate(&self, text: Option<String>) {
        self.estimate.set_subtitle(text.as_deref().unwrap_or("…"));
    }

    /// Exporting: progress shown, settings locked, the button stops it.
    pub fn set_busy(&self, busy: bool) {
        self.bar.set_visible(busy);
        self.bar.set_fraction(0.0);
        self.bar.set_text(Some(&gettext("Developing…")));
        for w in [self.format.upcast_ref::<gtk::Widget>(), self.quality.upcast_ref()] {
            w.set_sensitive(!busy);
        }
        self.go.set_label(&if busy { gettext("Stop") } else { gettext("Export…") });
        self.go.set_css_classes(if busy { &["destructive-action", "pill"] } else { &["suggested-action", "pill"] });
    }

    pub fn set_progress(&self, f: f32) {
        self.bar.set_fraction(f as f64);
        if f >= 1.0 {
            self.bar.set_text(Some(&gettext("Saving…")));
        }
    }
}

/// The format picked in `row` (its items are `PhotoFormat::ALL`).
fn selected(row: &adw::ComboRow) -> PhotoFormat {
    PhotoFormat::ALL.get(row.selected() as usize).copied().unwrap_or(PhotoFormat::Jpeg)
}
