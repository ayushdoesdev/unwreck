//! Quiescence detection for agents without hooks (`unwreck run -- aider`, `unwreck watch`).
//!
//! We do not try to snapshot on every write. We wait until the tree has been quiet
//! for `quiet` after a burst of activity, then call `on_quiet`, which takes one
//! incremental snapshot. See PLAN.md "Turn boundaries".
#![warn(missing_docs)]

use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Watcher settings for [`run`].
pub struct WatchConfig {
    /// Directory to watch recursively.
    pub root: PathBuf,
    /// Quiet period after the last relevant event before `on_quiet` runs.
    pub quiet: Duration,
    /// Paths (e.g. the store dir, `.git`) whose events never count as activity.
    pub ignore_prefixes: Vec<PathBuf>,
}

/// Watcher failures.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    /// `notify` could not start or watch the tree.
    #[error(transparent)]
    Notify(#[from] notify::Error),
}

/// Blocks until `stop` receives a message. `on_quiet` gets the number of relevant
/// events in the burst that just ended.
pub fn run(
    cfg: WatchConfig,
    stop: mpsc::Receiver<()>,
    mut on_quiet: impl FnMut(usize),
) -> Result<(), WatchError> {
    let (tx, rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(cfg.quiet, None, tx)?;
    debouncer.watch(&cfg.root, RecursiveMode::Recursive)?;
    loop {
        if stop.try_recv().is_ok() {
            return Ok(());
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(events)) => {
                let n = events
                    .iter()
                    .flat_map(|e| e.paths.iter())
                    .filter(|p| !is_ignored(p, &cfg.ignore_prefixes))
                    .count();
                if n > 0 {
                    on_quiet(n);
                }
            }
            Ok(Err(errs)) => {
                for e in errs {
                    eprintln!("unwreck: watch error: {e}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn is_ignored(p: &Path, prefixes: &[PathBuf]) -> bool {
    prefixes.iter().any(|pre| p.starts_with(pre)) || p.components().any(|c| c.as_os_str() == ".git")
}
