//! iwd credentials agent. When iwd needs a passphrase, this starts the GTK
//! prompt (`iwdtray-gui --prompt …`) as a short-lived process and relays its answer,
//! so the tray itself never loads GTK.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gio::glib::{self, Variant};
use gio::prelude::*;
use zeroize::Zeroizing;

use iwdtray_common::iwd::{self as shared, IFACE_AGENT, IFACE_AGENT_MANAGER, IWD_NAME, IWD_PATH};

use crate::export::Exported;

const ERROR_CANCELED: &str = "net.connman.iwd.Agent.Error.Canceled";

const AGENT_PATH: &str = "/io/github/bsd_source/iwdtray/Agent";

/// Where the prompt finds the network name and, for `password`, the username.
const ENV_SSID: &str = "IWDTRAY_SSID";
const ENV_USER: &str = "IWDTRAY_USER";

/// Upper bound on the prompt's answer; longer output is treated as invalid.
const MAX_ANSWER: usize = 4096;

const AGENT_XML: &str = r#"
<node>
  <interface name="net.connman.iwd.Agent">
    <method name="Release"/>
    <method name="RequestPassphrase">
      <arg name="network" type="o" direction="in"/>
      <arg name="passphrase" type="s" direction="out"/>
    </method>
    <method name="RequestPrivateKeyPassphrase">
      <arg name="network" type="o" direction="in"/>
      <arg name="passphrase" type="s" direction="out"/>
    </method>
    <method name="RequestUserNameAndPassword">
      <arg name="network" type="o" direction="in"/>
      <arg name="user" type="s" direction="out"/>
      <arg name="password" type="s" direction="out"/>
    </method>
    <method name="RequestUserPassword">
      <arg name="network" type="o" direction="in"/>
      <arg name="user" type="s" direction="in"/>
      <arg name="password" type="s" direction="out"/>
    </method>
    <method name="Cancel">
      <arg name="reason" type="s" direction="in"/>
    </method>
  </interface>
</node>"#;

/// Registered with iwd for as long as it is alive (and re-registered
/// whenever iwd restarts).
pub struct Agent {
    inner: Rc<Inner>,
    _exported: Exported,
    owner_handler: Option<glib::SignalHandlerId>,
}

struct Inner {
    manager: gio::DBusObjectManagerClient,
    prompt: PathBuf,
    /// Whether iwd has us as its agent: from registering until iwd stops
    /// or releases us.
    registered: Cell<bool>,
    pending: RefCell<Option<Pending>>,
    next_id: Cell<u64>,
}

/// The request currently being answered by a prompt process.
struct Pending {
    id: u64,
    invocation: gio::DBusMethodInvocation,
    process: gio::Subprocess,
}

/// What the prompt asks for, and how its answer maps to the D-Bus reply.
#[derive(Clone, Copy)]
enum Kind {
    Passphrase,
    PrivateKey,
    UserAndPassword,
    PasswordForUser,
}

impl Agent {
    /// Exports the agent on iwd's bus connection. `prompt` is the `iwdtray-gui`
    /// binary that shows the passphrase window.
    pub fn new(manager: &gio::DBusObjectManagerClient, prompt: &Path) -> Result<Self, glib::Error> {
        let inner = Rc::new(Inner {
            manager: manager.clone(),
            prompt: prompt.to_owned(),
            registered: Cell::new(false),
            pending: RefCell::new(None),
            next_id: Cell::new(0),
        });

        let weak = Rc::downgrade(&inner);
        let exported = Exported::new(
            &manager.connection(),
            AGENT_PATH,
            AGENT_XML,
            IFACE_AGENT,
            |b| {
                b.method_call(move |_, sender, _, _, method, params, invocation| {
                    match weak.upgrade() {
                        Some(inner) => inner.handle(sender, method, &params, invocation),
                        None => {
                            invocation.return_dbus_error(ERROR_CANCELED, "Agent is shutting down");
                        }
                    }
                })
            },
        )?;

        // Register now if iwd is running, and again each time it (re)starts.
        let weak = Rc::downgrade(&inner);
        let owner_handler = shared::on_owner_change(manager, move || {
            if let Some(inner) = weak.upgrade() {
                inner.on_iwd_owner_changed();
            }
        });
        inner.on_iwd_owner_changed();

        Ok(Self {
            inner,
            _exported: exported,
            owner_handler: Some(owner_handler),
        })
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.inner.cancel_pending("iwdtray is exiting");
        if let Some(id) = self.owner_handler.take() {
            self.inner.manager.disconnect(id);
        }
        if self.inner.registered.get() {
            call_agent_manager(&self.inner.manager.connection(), "UnregisterAgent");
        }
    }
}

impl Inner {
    fn on_iwd_owner_changed(&self) {
        let running = self.manager.name_owner().is_some();
        self.registered.set(running);
        if running {
            call_agent_manager(&self.manager.connection(), "RegisterAgent");
        } else {
            // iwd is gone; whatever it asked for no longer matters.
            self.cancel_pending("iwd stopped");
        }
    }

    fn handle(
        self: &Rc<Self>,
        sender: Option<&str>,
        method: &str,
        params: &Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        // Only iwd may ask for credentials; anyone else could be phishing.
        let Some(invocation) = shared::only_iwd(&self.manager, sender, invocation) else {
            return;
        };

        let kind = match method {
            "RequestPassphrase" => Kind::Passphrase,
            "RequestPrivateKeyPassphrase" => Kind::PrivateKey,
            "RequestUserNameAndPassword" => Kind::UserAndPassword,
            "RequestUserPassword" => Kind::PasswordForUser,
            "Cancel" => {
                self.cancel_pending("canceled by iwd");
                return invocation.return_value(None);
            }
            "Release" => {
                // iwd has already dropped us; there is nothing to unregister.
                self.registered.set(false);
                self.cancel_pending("agent released");
                return invocation.return_value(None);
            }
            _ => {
                return invocation.return_dbus_error(
                    "org.freedesktop.DBus.Error.UnknownMethod",
                    "Unknown method",
                );
            }
        };

        let network = params.child_value(0).str().unwrap_or_default().to_owned();
        let ssid = self.ssid(&network).unwrap_or(network);
        let user = match kind {
            Kind::PasswordForUser => params.child_value(1).str().map(str::to_owned),
            _ => None,
        };
        self.start_prompt(kind, &ssid, user.as_deref(), invocation);
    }

    fn ssid(&self, network: &str) -> Option<String> {
        let network = shared::proxy(&self.manager, network, shared::IFACE_NETWORK)?;
        shared::prop_str(&network, "Name")
    }

    fn start_prompt(
        self: &Rc<Self>,
        kind: Kind,
        ssid: &str,
        user: Option<&str>,
        invocation: gio::DBusMethodInvocation,
    ) {
        // iwd sends one request at a time, so there should be none pending;
        // if there is, the new one replaces it.
        self.cancel_pending("superseded by a new request");

        let kind_arg = match kind {
            Kind::Passphrase => "passphrase",
            Kind::PrivateKey => "private-key",
            Kind::UserAndPassword => "user-password",
            Kind::PasswordForUser => "password",
        };
        // The network and username go in the environment, which only this
        // user can read, not on the command line, which everyone can.
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDOUT_PIPE);
        launcher.setenv(ENV_SSID, ssid, true);
        match user {
            Some(user) => launcher.setenv(ENV_USER, user, true),
            None => launcher.unsetenv(ENV_USER),
        }
        let argv: [&std::ffi::OsStr; 3] = [
            self.prompt.as_os_str(),
            "--prompt".as_ref(),
            kind_arg.as_ref(),
        ];

        let process = match launcher.spawn(&argv) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Cannot start the passphrase prompt: {e}");
                return invocation
                    .return_dbus_error(ERROR_CANCELED, "Cannot show the passphrase prompt");
            }
        };
        let Some(stdout) = process.stdout_pipe() else {
            process.force_exit();
            return invocation
                .return_dbus_error(ERROR_CANCELED, "Cannot show the passphrase prompt");
        };

        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        *self.pending.borrow_mut() = Some(Pending {
            id,
            invocation,
            process,
        });

        // One spare byte detects answers longer than MAX_ANSWER.
        let buffer = Zeroizing::new(vec![0u8; MAX_ANSWER + 1]);
        let weak: Weak<Self> = Rc::downgrade(self);
        stdout.read_all_async(
            buffer,
            glib::Priority::DEFAULT,
            gio::Cancellable::NONE,
            move |result| {
                let Some(inner) = weak.upgrade() else { return };
                // Ignore answers to requests that were canceled or replaced.
                let pending = inner.pending.borrow_mut().take_if(|p| p.id == id);
                let Some(pending) = pending else { return };

                let answer = match result {
                    Ok((buffer, len, None)) if len <= MAX_ANSWER => reply_for(kind, &buffer[..len]),
                    Ok(_) => None,
                    Err((_, e)) => {
                        eprintln!("Cannot read the passphrase prompt's answer: {e}");
                        None
                    }
                };
                match answer {
                    Some(reply) => pending.invocation.return_value(Some(&reply)),
                    None => pending
                        .invocation
                        .return_dbus_error(ERROR_CANCELED, "Canceled by the user"),
                }
            },
        );
    }

    fn cancel_pending(&self, reason: &str) {
        // Taken out first, so no borrow is held while replying.
        let pending = self.pending.take();
        if let Some(pending) = pending {
            pending.process.force_exit();
            pending.invocation.return_dbus_error(ERROR_CANCELED, reason);
        }
    }
}

/// Builds the D-Bus reply from the prompt's NUL-separated answer. An empty
/// answer means the user canceled.
fn reply_for(kind: Kind, answer: &[u8]) -> Option<Variant> {
    let answer = std::str::from_utf8(answer).ok()?;
    if answer.is_empty() {
        return None;
    }
    match kind {
        Kind::Passphrase | Kind::PrivateKey | Kind::PasswordForUser => {
            (!answer.contains('\0')).then(|| (answer,).to_variant())
        }
        Kind::UserAndPassword => {
            let (user, password) = answer.split_once('\0')?;
            (!user.is_empty() && !password.contains('\0')).then(|| (user, password).to_variant())
        }
    }
}

fn call_agent_manager(connection: &gio::DBusConnection, method: &'static str) {
    let path = glib::variant::ObjectPath::try_from(AGENT_PATH).expect("valid object path");
    connection.call(
        Some(IWD_NAME),
        IWD_PATH,
        IFACE_AGENT_MANAGER,
        method,
        Some(&(path,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |result| {
            if let Err(e) = result {
                eprintln!("iwd {method} failed: {e}");
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{Kind, reply_for};

    #[test]
    fn passphrase_reply() {
        let v = reply_for(Kind::Passphrase, b"hunter22").unwrap();
        assert_eq!(v.type_().as_str(), "(s)");
        assert_eq!(v.child_value(0).str(), Some("hunter22"));
    }

    #[test]
    fn user_and_password_reply() {
        let v = reply_for(Kind::UserAndPassword, b"alice\0secret").unwrap();
        assert_eq!(v.type_().as_str(), "(ss)");
        assert_eq!(v.child_value(1).str(), Some("secret"));
    }

    #[test]
    fn malformed_answers_are_rejected() {
        assert!(reply_for(Kind::Passphrase, b"").is_none());
        assert!(reply_for(Kind::Passphrase, b"a\0b").is_none());
        assert!(reply_for(Kind::UserAndPassword, b"no-separator").is_none());
        assert!(reply_for(Kind::UserAndPassword, b"\0pass").is_none());
        assert!(reply_for(Kind::Passphrase, &[0xff, 0xfe]).is_none());
    }
}

/// Against a fake iwd on a private bus (see the `mock-iwd` crate). A small
/// shell script stands in for the GTK prompt.
#[cfg(test)]
mod iwd_tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use gio::glib;
    use gio::prelude::*;
    use mock_iwd::{HOME, MockIwd, run_until, wait_for, with_context};

    use super::{AGENT_PATH, Agent};

    const WAIT: Duration = Duration::from_secs(5);

    /// An executable stand-in for `iwdtray-gui --prompt`, deleted when dropped
    /// (even if the test fails).
    struct Prompt(PathBuf);

    impl std::ops::Deref for Prompt {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Prompt {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn fake_prompt(body: &str) -> Prompt {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "iwdtray-test-prompt-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake prompt");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        Prompt(path)
    }

    fn network() -> glib::Variant {
        (glib::variant::ObjectPath::try_from(HOME).expect("valid path"),).to_variant()
    }

    /// Starts the agent and waits until it has registered with the fake iwd.
    fn start(ctx: &glib::MainContext, iwd: &MockIwd, prompt: &Path) -> Agent {
        let manager = iwdtray_common::iwd::manager_on(&iwd.connect()).expect("object manager");
        let agent = Agent::new(&manager, prompt).expect("agent starts");
        assert!(run_until(ctx, WAIT, || iwd
            .calls()
            .iter()
            .any(|c| c.starts_with("AgentManager.RegisterAgent"))));
        agent
    }

    #[test]
    fn answers_iwd_with_the_prompts_passphrase() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let prompt = fake_prompt("printf hunter22");
            let _agent = start(ctx, &iwd, &prompt);
            let reply = wait_for(ctx, WAIT, &iwd.call_agent("RequestPassphrase", network()))
                .expect("agent replies");
            let reply = reply.expect("passphrase, not an error");
            assert_eq!(reply.child_value(0).str(), Some("hunter22"));
        });
    }

    #[test]
    fn the_prompt_gets_the_network_name_from_its_environment() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            // Only `--prompt passphrase` on the command line, which every
            // local user can read; the name comes from the environment.
            let prompt =
                fake_prompt(r#"[ "$*" = "--prompt passphrase" ] && printf %s "$IWDTRAY_SSID""#);
            let _agent = start(ctx, &iwd, &prompt);
            let reply = wait_for(ctx, WAIT, &iwd.call_agent("RequestPassphrase", network()))
                .expect("agent replies");
            let reply = reply.expect("the network name, not an error");
            assert_eq!(reply.child_value(0).str(), Some("Lighthouse"));
        });
    }

    #[test]
    fn registers_again_when_iwd_restarts() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let prompt = fake_prompt("printf hunter22");
            let _agent = start(ctx, &iwd, &prompt);
            iwd.stop_iwd();
            iwd.start_iwd();
            let registrations = || {
                iwd.calls()
                    .iter()
                    .filter(|c| c.starts_with("AgentManager.RegisterAgent"))
                    .count()
            };
            assert!(run_until(ctx, WAIT, || registrations() == 2));
        });
    }

    #[test]
    fn a_cancelled_prompt_is_reported_as_canceled() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            // The user closes the prompt: it exits without printing anything.
            let prompt = fake_prompt("exit 1");
            let _agent = start(ctx, &iwd, &prompt);
            let reply = wait_for(ctx, WAIT, &iwd.call_agent("RequestPassphrase", network()))
                .expect("agent replies");
            assert_eq!(reply.unwrap_err(), "net.connman.iwd.Agent.Error.Canceled");
        });
    }

    #[test]
    fn cancel_from_iwd_closes_the_prompt() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            // A prompt the user never answers.
            let prompt = fake_prompt("sleep 30");
            let _agent = start(ctx, &iwd, &prompt);
            let request = iwd.call_agent("RequestPassphrase", network());
            assert!(wait_for(ctx, Duration::from_millis(300), &request).is_none());
            let cancel = wait_for(
                ctx,
                WAIT,
                &iwd.call_agent("Cancel", ("timeout",).to_variant()),
            )
            .expect("Cancel is answered");
            assert!(cancel.is_ok());
            let reply = wait_for(ctx, WAIT, &request).expect("request ends");
            assert_eq!(reply.unwrap_err(), "net.connman.iwd.Agent.Error.Canceled");
        });
    }

    #[test]
    fn refuses_requests_that_are_not_from_iwd() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let prompt = fake_prompt("printf hunter22");
            let manager = iwdtray_common::iwd::manager_on(&iwd.connect()).expect("object manager");
            let _agent = Agent::new(&manager, &prompt).expect("agent starts");
            let agent_name = manager
                .connection()
                .unique_name()
                .expect("unique name")
                .to_string();

            // Another program on the bus pretends to be iwd.
            let intruder = iwd.connect();
            let (tx, rx) = std::sync::mpsc::channel();
            intruder.call(
                Some(&agent_name),
                AGENT_PATH,
                "net.connman.iwd.Agent",
                "RequestPassphrase",
                Some(&network()),
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                move |result| {
                    let _ = tx.send(result.map_err(|e| gio::DBusError::remote_error(&e)));
                },
            );
            let reply = wait_for(ctx, WAIT, &rx).expect("agent replies");
            assert_eq!(
                reply.unwrap_err().as_deref(),
                Some("org.freedesktop.DBus.Error.AccessDenied")
            );
        });
    }
}
