//! Where a provider keeps the user's account key on the host device (spec
//! #37): one private file per provider, `{data}/sandbox-keys/<id>.json`,
//! readable only by its owner. The engine decides where it lives; the
//! provider decides what's in it (an API key, or tokens it refreshes). It
//! never leaves this device, never goes into a sandbox and is never logged.

use std::io;
use std::path::{Path, PathBuf};

const DIR: &str = "sandbox-keys";

/// One provider's private credential slot on this device.
#[derive(Clone)]
pub struct KeySlot {
    path: PathBuf,
}

impl std::fmt::Debug for KeySlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeySlot").field("path", &self.path).finish()
    }
}

impl KeySlot {
    pub fn open(data_dir: &Path, provider_id: &str) -> Self {
        let name: String = provider_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Self {
            path: data_dir.join(DIR).join(format!("{name}.json")),
        }
    }

    /// What's kept, if anything.
    pub fn get(&self) -> Option<String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// Keep `value`, replacing what was there (atomically, owner-only).
    pub fn set(&self, value: &str) -> io::Result<()> {
        let dir = self.path.parent().expect("a slot is in a folder");
        std::fs::create_dir_all(dir)?;
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, value.as_bytes())?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Forget it (disconnect).
    pub fn clear(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// On Windows the data dir is in the user's profile, private to them already.
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_keeps_replaces_and_forgets_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let slot = KeySlot::open(dir.path(), "boat");
        assert_eq!(slot.get(), None);
        slot.set("one").unwrap();
        slot.set("two").unwrap();
        assert_eq!(
            KeySlot::open(dir.path(), "boat").get().as_deref(),
            Some("two")
        );
        assert_eq!(KeySlot::open(dir.path(), "docker").get(), None);
        slot.clear().unwrap();
        slot.clear().unwrap();
        assert_eq!(slot.get(), None);
        assert!(!format!("{slot:?}").contains("two"));
    }

    #[test]
    fn a_provider_id_cant_leave_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let slot = KeySlot::open(dir.path(), "../evil");
        slot.set("k").unwrap();
        assert!(dir.path().join(DIR).join("___evil.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_its_owner_can_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let slot = KeySlot::open(dir.path(), "boat");
        slot.set("k").unwrap();
        let mode = std::fs::metadata(dir.path().join(DIR).join("boat.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
