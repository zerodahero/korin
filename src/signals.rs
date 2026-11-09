//! Signal names, as written in `signal <NAME>` expect matches.

use std::str::FromStr;

use nix::sys::signal::Signal;

/// The signal for a bare name such as `TERM`, without the `SIG` prefix.
pub fn number(name: &str) -> Option<Signal> {
    Signal::from_str(&format!("SIG{name}")).ok()
}

/// The bare name of a signal, such as `TERM`.
pub fn name(signal: Signal) -> &'static str {
    signal.as_str().trim_start_matches("SIG")
}
