//! The `en-XA` leak test (`crate::i18n::leak_tests`) for every Settings
//! page: each draws only translated messages, around the section list and
//! footer, except for data such as theme and font names.
use super::*;
use crate::i18n::leak_tests::{assert_translated, draw, under_en_xa};
use gpui::{px, size};

/// Opens Settings on `section` under `en-XA` and checks every line drawn,
/// allowing the lines `data` lists.
fn check_page(section: Section, data: fn(&SettingsWindow, &App) -> Vec<String>) {
    under_en_xa("Settings en-XA leak", |cx, recorder| {
        let main = cx.add_window(crate::sidebar::layout_tests::fixture_window);
        let weak = cx.update(|cx| main.update(cx, |_, _, cx| cx.weak_entity()).unwrap());
        recorder.take();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = SettingsWindow::new(weak, cx);
            view.section = section;
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(1200.), px(3000.)));
        draw(cx);
        let data = cx.update(|_, cx| data(view.read(cx), cx));
        assert_translated(&format!("Settings > {section:?}"), &recorder.take(), &data);
    });
}

fn none(_: &SettingsWindow, _: &App) -> Vec<String> {
    Vec::new()
}

/// General: only messages.
fn general_data(_: &SettingsWindow, _: &App) -> Vec<String> {
    Vec::new()
}

/// Tabs & panes: each SSH host's name above its scrollback switch.
fn tabs_data(view: &SettingsWindow, _: &App) -> Vec<String> {
    view.remote_history
        .hosts()
        .iter()
        .map(|host| host.label.clone())
        .collect()
}

/// Notifications: the unit after the delay field.
// TODO(i18n): settings_window::ui::number takes its unit as a static str.
fn notifications_data(_: &SettingsWindow, _: &App) -> Vec<String> {
    vec!["s".into()]
}

/// Configuration: the paths of both files.
fn configuration_data(view: &SettingsWindow, _: &App) -> Vec<String> {
    let shared = view
        .shared
        .as_ref()
        .map(|shared| shared.path.display().to_string());
    std::iter::once(view.controls.local_path.clone())
        .chain(shared)
        .collect()
}

macro_rules! pages {
    ($($(#[$attr:meta])* $name:ident: $section:ident, $data:expr;)+) => {
        $(
            $(#[$attr])*
            #[test]
            fn $name() {
                check_page(Section::$section, $data);
            }
        )+
    };
}

pages! {
    general: General, general_data;
    #[ignore = "migrating"]
    theme: Theme, none;
    #[ignore = "migrating"]
    sidebar: Sidebar, none;
    #[ignore = "migrating"]
    indicators: Indicators, none;
    #[ignore = "migrating"]
    fonts: Fonts, none;
    #[ignore = "migrating"]
    status_bar: StatusBar, none;
    tabs: Tabs, tabs_data;
    code: Code, none;
    notifications: Notifications, notifications_data;
    integrations: Integrations, none;
    #[ignore = "migrating"]
    plugins: Plugins, none;
    #[cfg(feature = "cloud")]
    #[ignore = "migrating"]
    cloud_devices: CloudDevices, none;
    remote_control: RemoteControl, none;
    configuration: Configuration, configuration_data;
}
