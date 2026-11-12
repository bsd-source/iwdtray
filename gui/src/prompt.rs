//! A small window asking for a network's credentials.

use std::cell::RefCell;
use std::io::Write;
use std::process::ExitCode;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use zeroize::Zeroizing;

use crate::config::WindowConfig;
use crate::{hotspot, style, widgets};

type Answer = Rc<RefCell<Option<Zeroizing<String>>>>;

pub enum Request<'a> {
    Passphrase { ssid: &'a str },
    PrivateKey { ssid: &'a str },
    UserPassword { ssid: &'a str },
    Password { ssid: &'a str, user: &'a str },
}

impl Request<'_> {
    fn heading(&self) -> String {
        match self {
            Self::Passphrase { ssid }
            | Self::UserPassword { ssid }
            | Self::Password { ssid, .. } => format!("Enter the password for “{ssid}”"),
            Self::PrivateKey { ssid } => format!("Enter the private key passphrase for “{ssid}”"),
        }
    }

    fn min_len(&self) -> usize {
        // WPA passphrases are at least 8 characters; iwd checks the rest.
        if matches!(self, Self::Passphrase { .. }) {
            *hotspot::PASSWORD_LEN.start()
        } else {
            1
        }
    }
}

/// Shows the prompt; on success writes the answer to stdout and returns 0.
pub fn run(request: &Request) -> ExitCode {
    if let Err(e) = gtk::init() {
        eprintln!("Cannot initialise GTK: {e}");
        return ExitCode::from(2);
    }

    // Same palette as the main window.
    let config = WindowConfig::load();
    let _css = style::install(config.palette(config.dark), config.dark);

    let main_loop = glib::MainLoop::new(None, false);
    let answer = Answer::default();
    let form = Form::new(request);
    form.connect_signals(request.min_len(), &answer);
    form.window.connect_close_request({
        let main_loop = main_loop.clone();
        move |_| {
            main_loop.quit();
            glib::Propagation::Proceed
        }
    });

    form.window.present();
    main_loop.run();

    let Some(answer) = answer.borrow_mut().take() else {
        return ExitCode::from(1);
    };
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = stdout
        .write_all(answer.as_bytes())
        .and_then(|()| stdout.flush())
    {
        eprintln!("Cannot write the answer: {e}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

struct Form {
    window: gtk::Window,
    user_entry: Option<gtk::Entry>,
    password: gtk::PasswordEntry,
    cancel: gtk::Button,
    connect: gtk::Button,
}

impl Form {
    fn new(request: &Request) -> Self {
        let window = gtk::Window::builder().default_width(400).build();
        let grid = gtk::Grid::builder()
            .row_spacing(10)
            .column_spacing(12)
            .build();
        grid.add_css_class("prompt");

        let heading = gtk::Label::builder()
            .label(request.heading())
            .xalign(0.0)
            .build();
        heading.add_css_class("ssid");
        heading.add_css_class("prompt-heading");
        grid.attach(&heading, 0, 0, 2, 1);

        let mut row = 1;
        let mut add_row = |label: &str, widget: &gtk::Widget| {
            grid.attach(&widgets::label(label, "readout-label"), 0, row, 1, 1);
            grid.attach(widget, 1, row, 1, 1);
            row += 1;
        };

        let user_entry = match request {
            Request::UserPassword { .. } => {
                let entry = gtk::Entry::builder()
                    .hexpand(true)
                    .activates_default(true)
                    .build();
                add_row("USERNAME", entry.upcast_ref());
                Some(entry)
            }
            Request::Password { user, .. } => {
                let label = gtk::Label::builder().label(*user).xalign(0.0).build();
                add_row("USERNAME", label.upcast_ref());
                None
            }
            Request::Passphrase { .. } | Request::PrivateKey { .. } => None,
        };

        let password = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .activates_default(true)
            .hexpand(true)
            .build();
        add_row("PASSWORD", password.upcast_ref());

        let (cancel, connect) = widgets::dialog(&window, &grid, "Connect");
        connect.set_sensitive(false);
        window.set_default_widget(Some(&connect));

        Self {
            window,
            user_entry,
            password,
            cancel,
            connect,
        }
    }

    /// Enables Connect only once the answer could be valid, and stores the
    /// answer in `answer` when it is clicked.
    fn connect_signals(&self, min_len: usize, answer: &Answer) {
        let update_sensitive = {
            let (connect, password, user_entry) = (
                self.connect.clone(),
                self.password.clone(),
                self.user_entry.clone(),
            );
            move || {
                let user_ok = user_entry.as_ref().is_none_or(|e| !e.text().is_empty());
                connect.set_sensitive(user_ok && widgets::secret_len(&password) >= min_len);
            }
        };
        let u = update_sensitive.clone();
        self.password.connect_changed(move |_| u());
        if let Some(entry) = &self.user_entry {
            entry.connect_changed(move |_| update_sensitive());
        }

        self.connect.connect_clicked({
            let (window, password, user_entry, answer) = (
                self.window.clone(),
                self.password.clone(),
                self.user_entry.clone(),
                Rc::clone(answer),
            );
            move |_| {
                // Sized up front (four bytes per character at most), as
                // growing would leave unwiped copies behind.
                let user_len = user_entry.as_ref().map_or(0, |e| e.text().len() + 1);
                let len = user_len + 4 * widgets::secret_len(&password);
                let mut out = Zeroizing::new(String::with_capacity(len));
                if let Some(entry) = &user_entry {
                    out.push_str(&entry.text());
                    out.push('\0');
                }
                out.push_str(&password.text());
                *answer.borrow_mut() = Some(out);
                window.close();
            }
        });

        let window = self.window.clone();
        self.cancel.connect_clicked(move |_| window.close());
    }
}
