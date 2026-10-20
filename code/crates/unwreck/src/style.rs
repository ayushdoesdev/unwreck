//! Color that respects `NO_COLOR` and non-TTY stdout.
use std::io::{IsTerminal, stdout};

/// True when ANSI colors should be used.
pub fn enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && stdout().is_terminal()
}

/// Wrap `s` in an SGR sequence when [`enabled`].
pub fn wrap(code: &str, s: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

/// Color an A/M/D status character for `unwreck diff`.
pub fn status(ch: char) -> String {
    match ch {
        'A' => wrap("32", "A"),
        'M' => wrap("33", "M"),
        'D' => wrap("31", "D"),
        _ => ch.to_string(),
    }
}
