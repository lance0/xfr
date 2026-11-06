//! Shared by the integration tests that spawn the `xfr` binary.

use std::process::Command;

/// A `Command` for the `xfr` binary that ignores the developer's environment:
/// every `XFR_*` variable and the other variables xfr reads are removed, the
/// update check is off, and the per-user config and prefs directories
/// (`XDG_CONFIG_HOME` on Linux, `$HOME/Library/Application Support` on
/// macOS) point at a fresh empty directory. Tests that exercise env or config
/// set it on the returned command. `/etc/xfr/config.toml` is still searched.
///
/// Wrap it with `tokio::process::Command::from` for async tests.
pub fn xfr_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xfr"));
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("XFR_")) {
            command.env_remove(key);
        }
    }
    // ponytail: the directory outlives the child under target/tmp until
    // `cargo clean`; a guard type would have to thread through every caller.
    let home = tempfile::Builder::new()
        .prefix("xfr-env-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
        .keep();
    command
        .env_remove("DO_NOT_TRACK")
        .env_remove("NO_COLOR")
        .env_remove("RUST_LOG")
        .env("XFR_NO_UPDATE_CHECK", "1")
        .env("XDG_CONFIG_HOME", &home)
        .env("HOME", &home);
    command
}
