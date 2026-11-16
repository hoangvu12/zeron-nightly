//! `zeron-engine watchdog`: keeps `zeron-engine headless` running where no
//! service manager does. A sandbox has no systemd (spec #37,
//! docs/design/cloud-byo.md "Lifecycle": "Engine crashes in the sandbox →
//! restarted by a watchdog"). The device lending the sandbox its sign-in
//! starts it detached, with the engine's environment, and the engine
//! inherits that environment on every restart.
//!
//! It restarts the engine whenever it exits, however it exits, waiting a
//! little longer each time it dies young. Only one watchdog runs per data
//! folder: a second one finds the first and leaves.

use std::fs::File;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

/// How long it waits before a restart.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    /// After a first crash, or one that ran long enough to have settled.
    pub first: Duration,
    /// It doubles up to this while the engine keeps dying young.
    pub max: Duration,
    /// An engine that ran this long had settled: the wait starts over.
    pub settled: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            max: Duration::from_secs(30),
            settled: Duration::from_secs(60),
        }
    }
}

/// The lock that makes one watchdog per data folder.
const LOCK_FILE: &str = "watchdog.lock";

/// While this file is in the data folder, an engine that exits isn't started
/// again: `zeron-engine forget-device` writes it before it stops the engine
/// (Save as template). Same name as `zeron_engine::sandboxes::WATCHDOG_STOP_FILE`.
pub const STOP_FILE: &str = "watchdog.stop";

/// `zeron-engine watchdog`: this executable's `headless`, kept running.
pub fn run(data_dir: &Path) -> anyhow::Result<()> {
    let Some(_lock) = hold(data_dir)? else {
        tracing::info!("watchdog: another one already keeps the engine running");
        return Ok(());
    };
    // Started again (after a Save as template): it's wanted running now.
    clear_stop(data_dir);
    let exe = std::env::current_exe()?;
    keep_running(
        || std::process::Command::new(&exe).arg("headless").spawn(),
        Backoff::default(),
        || stopping(data_dir),
    );
    tracing::info!("watchdog: asked to stop; the engine stays stopped");
    Ok(())
}

fn stopping(data_dir: &Path) -> bool {
    data_dir.join(STOP_FILE).exists()
}

fn clear_stop(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(STOP_FILE));
}

/// This data folder's watchdog lock, unless another watchdog holds it. It's
/// released when the file closes, so a killed watchdog never keeps it.
pub fn hold(data_dir: &Path) -> anyhow::Result<Option<File>> {
    std::fs::create_dir_all(data_dir)?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(err)) => Err(err.into()),
    }
}

/// Start the engine with `start`, and again each time it exits, until
/// `stop` says so (checked after each exit).
pub fn keep_running(
    mut start: impl FnMut() -> std::io::Result<Child>,
    backoff: Backoff,
    mut stop: impl FnMut() -> bool,
) {
    let mut wait = backoff.first;
    loop {
        let started = Instant::now();
        match start() {
            Ok(mut child) => match child.wait() {
                Ok(status) => {
                    tracing::warn!(%status, "watchdog: the engine stopped; restarting it")
                }
                Err(err) => {
                    tracing::warn!(error = %err, "watchdog: lost the engine; restarting it")
                }
            },
            Err(err) => tracing::warn!(error = %err, "watchdog: couldn't start the engine"),
        }
        if stop() {
            return;
        }
        if started.elapsed() >= backoff.settled {
            wait = backoff.first;
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(backoff.max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that exits at once with `code`.
    fn exits(code: i32) -> std::process::Command {
        #[cfg(windows)]
        let mut command = {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", &format!("exit {code}")]);
            c
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", &format!("exit {code}")]);
            c
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null());
        command
    }

    #[test]
    fn an_engine_that_crashes_is_started_again_however_it_exits() {
        let quick = Backoff {
            first: Duration::from_millis(10),
            max: Duration::from_millis(40),
            settled: Duration::from_secs(60),
        };
        let codes = [101, 0, 70, 75];
        let started = std::cell::Cell::new(0);
        keep_running(
            || {
                started.set(started.get() + 1);
                exits(codes[(started.get() - 1) % codes.len()]).spawn()
            },
            quick,
            || started.get() == 5,
        );
        assert_eq!(started.get(), 5);
    }

    #[test]
    fn it_keeps_trying_when_the_engine_wont_start() {
        let quick = Backoff {
            first: Duration::from_millis(1),
            max: Duration::from_millis(2),
            settled: Duration::from_secs(60),
        };
        let tries = std::cell::Cell::new(0);
        keep_running(
            || {
                tries.set(tries.get() + 1);
                Err(std::io::Error::other("not there"))
            },
            quick,
            || tries.get() == 3,
        );
        assert_eq!(tries.get(), 3);
    }

    #[test]
    fn it_waits_longer_while_the_engine_keeps_dying_young() {
        let backoff = Backoff {
            first: Duration::from_millis(20),
            max: Duration::from_millis(80),
            settled: Duration::from_secs(60),
        };
        let at = std::cell::RefCell::new(Vec::new());
        keep_running(
            || {
                at.borrow_mut().push(Instant::now());
                exits(1).spawn()
            },
            backoff,
            || at.borrow().len() == 4,
        );
        let at = at.into_inner();
        let gaps: Vec<Duration> = at.windows(2).map(|w| w[1] - w[0]).collect();
        // 20, 40, 80 ms (and the process's own run time).
        assert!(gaps[0] >= Duration::from_millis(20), "{gaps:?}");
        assert!(gaps[1] >= Duration::from_millis(40), "{gaps:?}");
        assert!(gaps[2] >= Duration::from_millis(80), "{gaps:?}");
    }

    #[test]
    fn a_stop_file_ends_it_and_a_new_one_starts_without_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STOP_FILE), "left from before").unwrap();
        // A new watchdog clears what an old run left.
        clear_stop(dir.path());
        assert!(!stopping(dir.path()));
        // `zeron-engine forget-device` asks it to stop: once the engine
        // exits it isn't started again.
        let started = std::cell::Cell::new(0);
        keep_running(
            || {
                started.set(started.get() + 1);
                if started.get() == 2 {
                    std::fs::write(dir.path().join(STOP_FILE), "forget-device").unwrap();
                }
                exits(0).spawn()
            },
            Backoff {
                first: Duration::from_millis(1),
                max: Duration::from_millis(2),
                settled: Duration::from_secs(60),
            },
            || stopping(dir.path()),
        );
        assert_eq!(started.get(), 2);
    }

    #[test]
    fn one_watchdog_per_data_folder() {
        let dir = tempfile::tempdir().unwrap();
        let first = hold(dir.path()).unwrap();
        assert!(first.is_some());
        assert!(hold(dir.path()).unwrap().is_none());
        drop(first);
        assert!(hold(dir.path()).unwrap().is_some());
    }
}
