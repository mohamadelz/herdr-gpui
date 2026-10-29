use super::*;

#[test]
fn terminal_bidi_is_off_until_configured() -> anyhow::Result<()> {
    for config in [
        Config::default(),
        Config::parse("")?,
        Config::parse(DEFAULT_CONFIG)?,
    ] {
        assert!(!config.terminal_bidi);
    }
    let config = Config::parse("[terminal]\nbidi = true")?;
    assert!(config.terminal_bidi);
    assert!(config.unknown_keys.is_empty(), "{:?}", config.unknown_keys);
    assert!(!Config::parse("[terminal]\nbidi = false")?.terminal_bidi);
    Ok(())
}

#[test]
fn local_bidi_merges_over_managed_defaults() -> anyhow::Result<()> {
    let config = Config::parse_layers(
        [DEFAULT_CONFIG, "[terminal]\nbidi = true"],
        &Daemon::default(),
    )?;
    assert!(config.terminal_bidi);
    assert_eq!(config.terminal.size, 14.0);
    Ok(())
}

#[test]
fn rejects_a_bidi_that_is_not_a_boolean() {
    for value in ["1", "'true'", "[true]", "{}"] {
        assert!(
            matches!(
                Config::parse(&format!("[terminal]\nbidi = {value}")),
                Err(crate::Error::Settings(Error::InvalidTerminalBidi))
            ),
            "accepted bidi = {value}"
        );
    }
}

#[test]
fn bidi_outside_terminal_is_reported_and_ignored() -> anyhow::Result<()> {
    let config = Config::parse("[sidebar]\nbidi = true\n[tabs]\nbidi = 1\n[ui]\nbidi = true")?;
    assert_eq!(
        config.unknown_keys,
        ["sidebar.bidi", "tabs.bidi", "ui.bidi"]
    );
    assert!(!config.terminal_bidi);
    Ok(())
}
