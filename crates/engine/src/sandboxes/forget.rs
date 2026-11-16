//! `zeron-engine forget-device`: Zeron in a sandbox forgets who it is before
//! the sandbox is saved as a template (spec #37, docs/design/cloud-byo.md
//! "Docker specifics": Save as template wipes Zeron's identity). Otherwise
//! every sandbox made from the template would start with the same device id
//! and they'd take each other's place on the relay (a host room closes the
//! older host with 4409), and whatever the engine kept on disk would go with
//! the template.
//!
//! It runs in the sandbox, as the engine's own command, because only the
//! engine knows its data folder and what's in it. In order:
//!
//! 1. the running engine is quiesced over its IPC; if a turn is running it
//!    refuses ([`Forgot::Busy`]) and nothing changes;
//! 2. its watchdog is told to stop restarting it ([`WATCHDOG_STOP_FILE`]),
//!    and the engine is stopped (`StopEngine`); both are waited for;
//! 3. everything in the data folder goes but what [`KEEP`] names.
//!
//! The device that saves then starts the engine again, which takes the
//! chat's device id afresh ([`super::take_device_id`]).

use std::path::Path;
use std::time::{Duration, Instant};

use crate::instance_lock::InstanceLock;

/// The subcommand: `zeron-engine forget-device`.
pub const FORGET_DEVICE: &str = "forget-device";

/// Its exit status when the engine is busy (EX_TEMPFAIL, as `zeron quiesce`).
pub const FORGET_BUSY_EXIT: i32 = 75;

/// While this file is in the data folder, `zeron-engine watchdog` doesn't
/// restart the engine (`zeron_cli::watchdog`, which reads the same name).
pub const WATCHDOG_STOP_FILE: &str = "watchdog.stop";

/// What stays in the data folder: the installed release (so a sandbox from
/// the template skips Install Zeron), the user's step scripts, the user's
/// work (worktrees, scratch folders and clones live under `~/.zeron` too),
/// and agent preferences. Everything else is Zeron's identity or data: the
/// device id, the stores, sign-in and plugin keys, IPC tokens, logs, run
/// files, and whatever later versions add, which goes by default.
pub const KEEP: &[&str] = &[
    "app",
    "steps",
    "worktrees",
    "scratch",
    "repos",
    "harness-prefs.json",
    "worktree-settings.json",
];

/// How long the engine and its watchdog get to stop.
const STOP_DEADLINE: Duration = Duration::from_secs(60);

/// How `forget-device` ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Forgot {
    /// Stopped and wiped.
    Done,
    /// A turn is running (or other work the engine won't drop): nothing
    /// was changed.
    Busy(String),
}

/// Stop Zeron on `data_dir` (listening for IPC on `ipc_port`) and wipe its
/// identity.
pub async fn forget_device(data_dir: &Path, ipc_port: u16) -> anyhow::Result<Forgot> {
    if let Ok(client) = zeron_rpc::connect_ipc(ipc_port, Some(data_dir)).await {
        let reply = client
            .call(zeron_rpc::methods::QUIESCE_FOR_SLEEP, serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("QuiesceForSleep failed: {e}"))?;
        if reply["quiesced"].as_bool() != Some(true) {
            let why = reply["busy"].as_str().unwrap_or("the engine is busy");
            return Ok(Forgot::Busy(why.to_owned()));
        }
        std::fs::write(data_dir.join(WATCHDOG_STOP_FILE), b"forget-device\n")?;
        // It answers, then stops.
        let _ = client
            .call(zeron_rpc::methods::STOP_ENGINE, serde_json::json!({}))
            .await;
    } else {
        // Not answering (between restarts): the watchdog mustn't start it.
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(data_dir.join(WATCHDOG_STOP_FILE), b"forget-device\n")?;
    }
    let deadline = Instant::now() + STOP_DEADLINE;
    while InstanceLock::holder(data_dir).is_some() || watchdog_running(data_dir) {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "Zeron didn't stop within {} s, so its identity wasn't wiped",
                STOP_DEADLINE.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    forget_device_files(data_dir)?;
    Ok(Forgot::Done)
}

/// Whether a watchdog holds `data_dir`'s watchdog lock.
fn watchdog_running(data_dir: &Path) -> bool {
    let Ok(file) = std::fs::File::options()
        .write(true)
        .open(data_dir.join("watchdog.lock"))
    else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// Wipe Zeron's identity and data from `data_dir`, keeping what [`KEEP`]
/// names. Zeron must not be running on it. Refuses a folder that isn't
/// plainly a data folder of its own (a home folder, a root).
pub fn forget_device_files(data_dir: &Path) -> std::io::Result<()> {
    let refuse = |why: &str| {
        Err(std::io::Error::other(format!(
            "won't wipe {}: {why}",
            data_dir.display()
        )))
    };
    if data_dir.parent().is_none() {
        return refuse("it's a root");
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(home) = home
        && same_dir(data_dir, Path::new(&home))
    {
        return refuse("it's the home folder");
    }
    let entries = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if KEEP.iter().any(|keep| name == *keep) {
            continue;
        }
        let path = entry.path();
        // A symlink goes as a link, never what it points at.
        let outcome = if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(err) = outcome
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(std::io::Error::other(format!(
                "couldn't remove {}: {err}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, rel: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, rel).unwrap();
    }

    #[test]
    fn identity_and_data_go_the_release_and_the_users_work_stay() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join(".zeron");
        for gone in [
            "device-id",
            "device-id.lock",
            ".device-id.tmp-1-x",
            "session.json",
            "local-profile.json",
            "orgs/org_1/user_1/docs.sqlite3",
            "profiles/local/docs.sqlite3",
            "uploads/a.png",
            "ipc-token",
            "ipc-token-agent",
            "engine.lock",
            "plugin-signing-key",
            "plugin-keys.json",
            "runner.json",
            "cloud-session.json",
            "update-transaction.json",
            "logs/zeron-headless.log",
            "run/zeron-engine.pid",
            "sandboxes.json",
            "keys/docker",
            "watchdog.lock",
            "watchdog.stop",
            "agent-accounts/claude.json",
            "something-a-later-version-adds",
        ] {
            touch(&data, gone);
        }
        for kept in [
            "app/1.2.3/zeron-engine",
            "steps/after-setup/10-tools.sh",
            "worktrees/reportly/x/README.md",
            "scratch/s/notes.md",
            "repos/r/README.md",
            "harness-prefs.json",
            "worktree-settings.json",
        ] {
            touch(&data, kept);
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(data.join("app/1.2.3"), data.join("app/current")).unwrap();

        forget_device_files(&data).unwrap();

        let mut left: Vec<String> = std::fs::read_dir(&data)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "app",
                "harness-prefs.json",
                "repos",
                "scratch",
                "steps",
                "worktree-settings.json",
                "worktrees"
            ]
        );
        assert!(data.join("app/1.2.3/zeron-engine").exists());
        #[cfg(unix)]
        assert!(data.join("app/current/zeron-engine").exists());
        // Again: nothing more to do.
        forget_device_files(&data).unwrap();
        // A data folder that isn't there yet is fine.
        forget_device_files(&dir.path().join("none")).unwrap();
    }

    #[test]
    fn it_never_wipes_a_home_folder() {
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            return;
        };
        let err = forget_device_files(Path::new(&home)).unwrap_err();
        assert!(err.to_string().contains("home"), "{err}");
    }

    #[test]
    fn with_no_engine_running_it_just_wipes() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "device-id");
        touch(dir.path(), "app/1/zeron-engine");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Port 9: nothing listens there.
        let forgot = runtime.block_on(forget_device(dir.path(), 9)).unwrap();
        assert_eq!(forgot, Forgot::Done);
        assert!(!dir.path().join("device-id").exists());
        assert!(!dir.path().join(WATCHDOG_STOP_FILE).exists());
        assert!(dir.path().join("app/1/zeron-engine").exists());
    }

    #[test]
    fn a_running_watchdog_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let lock = std::fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.path().join("watchdog.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        // (Another open file description: as another process sees it.)
        assert!(watchdog_running(dir.path()));
        lock.unlock().unwrap();
        assert!(!watchdog_running(dir.path()));
    }
}
