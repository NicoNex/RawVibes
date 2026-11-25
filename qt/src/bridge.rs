//! The one QObject QML talks to: the editor's state as JSON (`state`), commands (`cmd`), translations (`tr`),
//! and the image provider frames are stored in.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qqmlapplicationengine.h");
        type QQmlApplicationEngine = cxx_qt_lib::QQmlApplicationEngine;
        include!("images.h");
        /// Stores an RGBA8 (RGBA16F with `half`) frame under `key` for image://rv/<key>/<generation>.
        fn rv_set_image(key: &str, pixels: &[u8], width: u32, height: u32, half: bool);
        fn rv_install_images(engine: Pin<&mut QQmlApplicationEngine>);
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, state)]
        type Backend = super::BackendRust;

        /// A command from the UI: its name and JSON arguments.
        #[qinvokable]
        fn cmd(self: Pin<&mut Backend>, name: &QString, args: &QString);

        /// `text` translated with the GTK UI's gettext catalogue.
        #[qinvokable]
        fn tr(self: &Backend, text: &QString) -> QString;

        /// A message for the user; `action` labels an optional button that sends command `id`.
        #[qsignal]
        fn toast(self: Pin<&mut Backend>, text: QString, action: QString, id: i32);

        /// Asks QML to open a dialog (`kind`: open, save, folder…) with JSON options.
        #[qsignal]
        fn dialog(self: Pin<&mut Backend>, kind: QString, options: QString);
    }

    impl cxx_qt::Threading for Backend {}
    impl cxx_qt::Initialize for Backend {}
}

use crate::app::{App, Msg};
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;

pub use qobject::{rv_install_images, rv_set_image};

#[derive(Default)]
pub struct BackendRust {
    state: QString,
    app: Option<Box<App>>,
}

/// Where worker threads and timers send their messages: the Qt thread, through the backend.
#[derive(Clone)]
pub struct Tx(cxx_qt::CxxQtThread<qobject::Backend>);

impl Tx {
    pub fn send(&self, msg: Msg) {
        // The backend is gone only while the app quits: nothing left to tell.
        let _ = self.0.queue(move |b| b.handle(msg));
    }
}

impl cxx_qt::Initialize for qobject::Backend {
    fn initialize(mut self: Pin<&mut Self>) {
        let tx = Tx(self.qt_thread());
        let mut app = App::new(tx);
        // QML's first bindings read a whole state, not an empty one.
        let (state, ..) = app.publish();
        self.as_mut().rust_mut().app = Some(Box::new(app));
        if let Some(state) = state {
            self.set_state(QString::from(&state));
        }
    }
}

impl qobject::Backend {
    fn cmd(self: Pin<&mut Self>, name: &QString, args: &QString) {
        let args: serde_json::Value = serde_json::from_str(&args.to_string()).unwrap_or(serde_json::Value::Null);
        let name = name.to_string();
        match crate::app::command(&name, &args) {
            // Closing joins the GPU thread now: the event loop may not run again.
            Some(msg) if name == "close" => self.handle(msg),
            // Queued, not run now: QML may be evaluating a binding that reads the state this changes.
            Some(msg) => {
                let _ = self.qt_thread().queue(move |b| b.handle(msg));
            }
            None => eprintln!("rawvibes-qt: unknown command {name} {args}"),
        }
    }

    fn tr(&self, text: &QString) -> QString {
        QString::from(&crate::i18n::gettext(&text.to_string()))
    }

    /// Runs `msg` through the editor, then publishes what it changed.
    pub fn handle(mut self: Pin<&mut Self>, msg: Msg) {
        let Some(mut app) = self.as_mut().rust_mut().app.take() else { return };
        app.update(msg);
        let (state, toasts, dialogs) = app.publish();
        self.as_mut().rust_mut().app = Some(app);
        if let Some(state) = state {
            self.as_mut().set_state(QString::from(&state));
        }
        for (text, action, id) in toasts {
            self.as_mut().toast(QString::from(&text), QString::from(&action), id);
        }
        for (kind, options) in dialogs {
            self.as_mut().dialog(QString::from(&kind), QString::from(&options));
        }
    }
}
