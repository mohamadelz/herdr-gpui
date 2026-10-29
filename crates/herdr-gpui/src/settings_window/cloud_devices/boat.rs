//! The Boat tab of Settings > Cloud devices. Account fields save to `[boat]`;
//! its API key saves to the credential store. API reads and removals stay off
//! the UI thread, and creation jobs belong to the main window.

use super::{
    super::{Section, SettingsWindow},
    follow_config, input,
};
use crate::{
    boat::{
        SandboxCount, Settings,
        setup::{self, AddRequest, Key, Overview, Target},
    },
    cloud::{
        CloudProvider, SavedDevice,
        worker::{self, Worker},
    },
    config::{BoatFields, Config},
    github::Store,
    search_input::SearchInput,
};
use gpui::{prelude::*, *};
use secrecy::SecretString;

const FIELDS: [(&str, &str); 5] = [
    ("API URL", crate::boat::DEFAULT_API_URL),
    (
        "Organization",
        "Optional; the active personal or organization wallet if empty",
    ),
    ("Machine type", "Optional: small, default, large, or xlarge"),
    (
        "Environment",
        "Optional; the account's default environment if empty",
    ),
    ("Auto-stop after", "never, or 1–2592000 seconds"),
];

fn sandbox_status(count: SandboxCount) -> String {
    match count {
        SandboxCount::Exact(1) => "Connected. The key sees 1 sandbox.".into(),
        SandboxCount::Exact(count) => format!("Connected. The key sees {count} sandboxes."),
        SandboxCount::AtLeast(count) => {
            format!("Connected. The key sees {count} or more sandboxes.")
        }
    }
}

enum Account {
    Checking,
    Known(Box<Overview>),
}

pub(in crate::settings_window) struct BoatCard {
    fields: [Entity<SearchInput>; 5],
    loaded: BoatFields,
    key: Entity<SearchInput>,
    account: Account,
    install: bool,
    /// Run the setup script on the next device made here, when one is set.
    setup: bool,
    message: Option<String>,
    job: Option<Worker>,
    confirm_forget: bool,
}

impl BoatCard {
    fn new(
        config: &Config,
        theme: &crate::config::Theme,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let loaded = loaded(config);
        let texts = texts(&loaded);
        let fields =
            std::array::from_fn(|index| input(FIELDS[index].1, texts[index], config, theme, cx));
        let key = input("boat_…", "", config, theme, cx);
        key.update(cx, |input, cx| input.set_masked(true, cx));
        Self {
            fields,
            loaded,
            key,
            account: Account::Checking,
            install: true,
            setup: true,
            message: None,
            job: None,
            confirm_forget: false,
        }
    }

    fn values(&self, cx: &App) -> BoatFields {
        let text = |index: usize| self.fields[index].read(cx).text().trim().to_owned();
        BoatFields {
            api_url: text(0),
            organization: text(1),
            machine_type: text(2),
            environment: text(3),
            auto_stop: text(4),
        }
    }

    fn devices(&self) -> &[SavedDevice] {
        match &self.account {
            Account::Known(overview) => &overview.devices,
            Account::Checking => &[],
        }
    }
}

fn loaded(config: &Config) -> BoatFields {
    let mut values = BoatFields::from_config(&config.boat);
    if values.api_url.is_empty() {
        values.api_url = crate::boat::DEFAULT_API_URL.into();
    }
    values
}

fn texts(values: &BoatFields) -> [&str; 5] {
    [
        &values.api_url,
        &values.organization,
        &values.machine_type,
        &values.environment,
        &values.auto_stop,
    ]
}

/// The setup script a Boat job carries. Only a new sandbox runs it: Finish
/// setup on a saved device, used to extend its auto-stop, would otherwise
/// run it again; the device's menu does that when asked.
fn job_script(target: &Target, script: Option<String>) -> Option<String> {
    matches!(target, Target::Create(_))
        .then_some(script)
        .flatten()
}

fn setup_running(cx: &App) -> bool {
    cx.windows().into_iter().any(|window| {
        window
            .downcast::<crate::HerdrWindow>()
            .is_some_and(|window| {
                window.read(cx).is_ok_and(|window| {
                    window
                        .cloud_jobs
                        .iter()
                        .any(|job| job.provider == CloudProvider::Boat)
                })
            })
    })
}

impl SettingsWindow {
    pub(super) fn open_boat_card(&mut self, cx: &mut Context<Self>) {
        if self.boat_card.is_none() {
            self.boat_card = Some(BoatCard::new(&self.config, &self.theme, cx));
        }
        self.refresh_boat(cx);
    }

    fn boat_settings(&self) -> Option<Settings> {
        self.config.boat.settings().ok().flatten()
    }

    fn boat_job(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(&dyn Fn() -> bool, &dyn Fn(crate::boat::Result<Overview>)) + Send + 'static,
    ) {
        let Some(card) = &mut self.boat_card else {
            return;
        };
        card.job = None;
        match worker::spawn(
            "herdr-boat-settings",
            cx,
            work,
            |this: &mut Self, update, _| this.apply_boat(update),
        ) {
            Ok(job) => {
                if let Some(card) = &mut self.boat_card {
                    card.job = Some(job);
                }
            }
            Err(error) => {
                tracing::error!(category = "boat_worker", error_kind = ?error.kind(), "Could not start Boat settings worker");
                if let Some(card) = &mut self.boat_card {
                    card.message = Some(crate::cloud::Error::Worker("settings").to_string());
                }
            }
        }
    }

    fn refresh_boat(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = self.boat_settings() else {
            if let Some(card) = &mut self.boat_card {
                card.job = None;
                card.account = Account::Known(Box::new(Overview {
                    key: Ok(Key::Missing),
                    sandboxes: None,
                    devices: Vec::new(),
                    pending: Ok(None),
                }));
            }
            return;
        };
        if let Some(card) = &mut self.boat_card {
            card.account = Account::Checking;
        }
        self.boat_job(cx, move |_, send| {
            send(setup::overview(&settings));
        });
    }

    fn apply_boat(&mut self, update: crate::boat::Result<Overview>) {
        let Some(card) = &mut self.boat_card else {
            return;
        };
        match update {
            Ok(overview) => card.account = Account::Known(Box::new(overview)),
            Err(error) => {
                card.account = Account::Known(Box::new(Overview {
                    key: Ok(Key::Missing),
                    sandboxes: None,
                    devices: Vec::new(),
                    pending: Ok(None),
                }));
                card.message = Some(error.to_string());
            }
        }
    }

    fn save_boat(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Some(card) = &mut self.boat_card else {
            return;
        };
        let values = card.values(cx);
        let existing = self.config.boat.clone();
        let key = card.key.read(cx).text().trim().to_owned();
        let key = (!key.is_empty()).then(|| SecretString::from(key));
        let store = Store::for_policy(existing.allow_plaintext_credentials);
        card.key.update(cx, SearchInput::clear);
        card.message = None;
        self.save_native(
            move || {
                Config::save_boat(&values, &existing)?;
                if let Some(key) = &key {
                    setup::save_key(store, Some(key))?;
                }
                Ok(())
            },
            cx,
        );
    }

    fn forget_boat_key(&mut self, cx: &mut Context<Self>) {
        let store = Store::for_policy(self.config.boat.allow_plaintext_credentials);
        // Credential mutations share the save lifecycle: replacement waits for
        // deletion, and account refreshes cannot drop a write's completion.
        self.save_native(move || Ok(setup::save_key(store, None)?), cx);
    }

    fn forget_boat_device(&mut self, id: String, cx: &mut Context<Self>) {
        if setup_running(cx) {
            return;
        }
        self.save_native(move || Ok(setup::forget_device(&id)?), cx);
    }

    fn create_boat_sandbox(&mut self, cx: &mut Context<Self>) {
        let name = self
            .boat_card
            .as_ref()
            .and_then(|card| match &card.account {
                Account::Known(overview) => overview.pending.as_ref().ok().and_then(Clone::clone),
                Account::Checking => None,
            })
            .unwrap_or_else(setup::random_name);
        self.start_boat_setup(Target::Create(name), cx);
    }

    fn forget_boat_pending(&mut self, cx: &mut Context<Self>) {
        if self.busy() || setup_running(cx) {
            return;
        }
        let Some(settings) = self.boat_settings() else {
            return;
        };
        let Some(card) = &mut self.boat_card else {
            return;
        };
        if !card.confirm_forget {
            card.confirm_forget = true;
            cx.notify();
            return;
        }
        card.confirm_forget = false;
        self.save_native(move || Ok(setup::forget_pending(&settings)?), cx);
    }

    fn start_boat_setup(&mut self, target: Target, cx: &mut Context<Self>) {
        if self.busy() || setup_running(cx) {
            return;
        }
        let Some(settings) = self.boat_settings() else {
            return;
        };
        let Some(card) = &mut self.boat_card else {
            return;
        };
        let name = match &target {
            Target::Create(name) => name.clone(),
            Target::Saved(device) => device.label.clone(),
        };
        let (install, run) = (card.install, card.setup);
        let script = job_script(&target, self.cloud_setup_script(CloudProvider::Boat, run));
        let request = AddRequest {
            settings,
            target,
            install,
        };
        let started = self.source.update(cx, |window, cx| {
            window.start_cloud_job(
                CloudProvider::Boat,
                name.clone(),
                script,
                move |cancelled, report| setup::add_device(request, cancelled, report),
                cx,
            )
        });
        if let Some(card) = &mut self.boat_card {
            card.message = Some(match started {
                Ok(Ok(())) => {
                    format!("Setting up {name}. The device picker shows its progress.")
                }
                Ok(Err(error)) => error.to_string(),
                Err(_) => "Open a main Herdr window to set up a sandbox.".into(),
            });
        }
        cx.notify();
    }

    pub(super) fn boat_config_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(card) = &mut self.boat_card {
            let loaded = loaded(&self.config);
            follow_config(&card.fields, texts(&card.loaded), texts(&loaded), cx);
            card.loaded = loaded;
            card.confirm_forget = false;
        }
        if self.boat_card.is_some() && self.section == Section::CloudDevices {
            self.refresh_boat(cx);
        }
    }

    pub(super) fn render_boat_card(&self, cx: &mut Context<Self>) -> Div {
        let Some(card) = &self.boat_card else {
            return div();
        };
        let ready = !self.busy();
        let running = setup_running(cx);
        let configured = self.boat_settings().is_some();
        let (status, key) = match &card.account {
            _ if !configured => (
                "Not configured. Check the API URL, add an API key, then save.".to_owned(),
                Some(Key::Missing),
            ),
            Account::Checking => ("Checking…".to_owned(), Some(Key::Missing)),
            Account::Known(overview) => (
                match (&overview.key, &overview.sandboxes) {
                    (Err(error), _) => format!("The API key could not be read: {error}"),
                    (Ok(Key::Missing), _) => "No API key saved.".to_owned(),
                    (_, Some(Ok(count))) => sandbox_status(*count),
                    (_, Some(Err(error))) => format!("The API key could not be used: {error}"),
                    (_, None) => "Checking…".to_owned(),
                },
                overview.key.as_ref().ok().copied(),
            ),
        };
        let usable = matches!(&card.account, Account::Known(overview)
            if matches!(overview.sandboxes, Some(Ok(_))));
        let key_note = match key {
            Some(Key::Environment) => "Set by HERDR_BOAT_API_KEY, which takes precedence.",
            Some(Key::Saved) => "Saved. Type a new key to replace it.",
            Some(Key::Missing) => "Not saved yet. Create one in the Boat dashboard under API Keys.",
            None => "Credential storage is unavailable. Saved devices can still be removed.",
        };
        let mut account = self
            .control_card("Boat")
            .debug_selector(|| "cloud-boat-card".into())
            .child(self.control_note(
                "Create Boat sandboxes and use them as devices. Herdr authorizes a GUI-owned SSH key and pins each machine's host key from Boat's authenticated API.",
            ))
            .child(div().min_w_0().child(status));
        for (index, (label, _)) in FIELDS.iter().enumerate() {
            account = account.child(self.cloud_field(*label, card.fields[index].clone().into()));
            if index == 0 {
                account = account
                    .child(self.cloud_field("API key", card.key.clone().into()))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .child(self.control_note(key_note))
                            .when(key == Some(Key::Saved), |row| {
                                row.child(
                                    self.cloud_button("cloud-boat-forget-key", "Forget key", ready)
                                        .when(ready, |button| {
                                            button.on_click(cx.listener(|this, _, _, cx| {
                                                this.forget_boat_key(cx)
                                            }))
                                        }),
                                )
                            }),
                    );
            }
        }
        account = account.child(self.control_note(
            "Use never to disable auto-stop: billing continues until you stop the sandbox in Boat. A number sets a fixed deadline, even during SSH work (trials require 1–7200 seconds). Applies on create and resume. For a running device, save then use Finish setup to extend from now or disable auto-stop.",
        ));
        account = account.child(
            div().flex().justify_end().child(
                self.cloud_button("cloud-boat-save", "Save", ready)
                    .debug_selector(|| "cloud-boat-save".into())
                    .when(ready, |button| {
                        button.on_click(cx.listener(|this, _, _, cx| this.save_boat(cx)))
                    }),
            ),
        );
        let setup = ready
            && configured
            && matches!(key, Some(Key::Saved | Key::Environment))
            && crate::cloud::unavailable().is_none()
            && !running;
        let pending = match &card.account {
            Account::Known(overview) => overview
                .pending
                .as_ref()
                .ok()
                .and_then(|name| name.as_deref()),
            Account::Checking => None,
        };
        let create = setup && (usable || pending.is_some());
        let create_label = if pending.is_some() {
            "Recover creation"
        } else {
            "Create sandbox"
        };
        let mut sandboxes = self
            .control_card("New sandbox")
            .debug_selector(|| "cloud-boat-new".into())
            .child(self.control_note(
                "Creates a sandbox with a generated name. Its device is saved before setup so an interruption can be retried without creating another sandbox.",
            ))
            .child(
                self.control_switch(
                    "cloud-boat-install",
                    "Install Herdr in it when missing",
                    card.install,
                    ready,
                )
                .when(ready, |row| {
                    row.on_click(cx.listener(|this, _, _, cx| {
                        if let Some(card) = &mut this.boat_card {
                            card.install = !card.install;
                        }
                        cx.notify();
                    }))
                }),
            )
            .children(self.cloud_setup_switch(
                CloudProvider::Boat,
                card.setup,
                ready,
                |this| {
                    if let Some(card) = &mut this.boat_card {
                        card.setup = !card.setup;
                    }
                },
                cx,
            ))
            .child(
                div().flex().justify_end().child(
                    self.cloud_button("cloud-boat-create", create_label, create)
                        .debug_selector(|| "cloud-boat-create".into())
                        .when(create, |button| {
                            button.on_click(
                                cx.listener(|this, _, _, cx| this.create_boat_sandbox(cx)),
                            )
                        }),
                ),
            );
        if let Some(message) = &card.message {
            sandboxes = sandboxes.child(div().min_w_0().child(message.clone()));
        }
        if let Account::Known(overview) = &card.account {
            match &overview.pending {
                Ok(Some(name)) => {
                    sandboxes = sandboxes.child(self.control_note(format!(
                        "Pending creation: {name}. Recover creation reuses the original request and options, even after restarting. Recovery expires after 23 hours; it never silently creates a replacement.",
                    )));
                }
                Err(error) => sandboxes = sandboxes.child(self.control_note(error.to_string())),
                Ok(None) => {}
            }
            if !matches!(&overview.pending, Ok(None)) {
                let enabled = ready && !running;
                let forget_label = if card.confirm_forget {
                    "I checked Boat; forget request"
                } else {
                    "Forget pending request…"
                };
                sandboxes = sandboxes
                    .when(card.confirm_forget, |card| card.child(self.control_note(
                        "Check the Boat dashboard first: the original sandbox may exist and still be billed. Forgetting only removes this local request. Creating again can create a second sandbox.",
                    )))
                    .child(self.cloud_button("cloud-boat-forget-pending", forget_label, enabled)
                        .when(enabled, |button| button.on_click(cx.listener(|this, _, _, cx| this.forget_boat_pending(cx)))));
            }
        }
        let mut devices = self
            .control_card("Boat devices")
            .debug_selector(|| "cloud-boat-devices".into())
            .child(self.control_note(
                "Finish setup reuses the sandbox and applies Auto-stop after from now, extending a timed deadline or disabling it. The installation switch above also applies here; the setup script does not run again.",
            ));
        if card.devices().is_empty() {
            devices = devices.child(self.control_note("None yet."));
        }
        for (index, device) in card.devices().iter().enumerate() {
            let saved = device.clone();
            devices = devices.child(
                self.cloud_device_row(
                    index,
                    device,
                    format!("{} · {}", device.machine, device.session),
                    ready && !running,
                    Self::forget_boat_device,
                    cx,
                )
                .flex_wrap()
                .child(
                    self.cloud_button(
                        SharedString::from(format!("cloud-boat-finish-{index}")),
                        "Finish setup",
                        setup,
                    )
                    .debug_selector(move || format!("cloud-boat-finish-{index}"))
                    .when(setup, |button| {
                        button.on_click(cx.listener(move |this, _, _, cx| {
                            this.start_boat_setup(Target::Saved(saved.clone()), cx);
                        }))
                    }),
                ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .min_w_0()
            .child(account)
            .child(sandboxes)
            .child(devices)
    }
}

#[cfg(test)]
mod tests;
