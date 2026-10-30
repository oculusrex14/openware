//! Transactional save (P5.C5): every kernel write lands via
//! write-aside -> fsync -> atomic rename, so a crash mid-save leaves the
//! previous complete state (the CW-native `SaveChanges`/`.csl` change-file
//! lane is a separate, tracked surface — see the spec, §5).
//!
//! `commit_if_changed` makes the no-op save a true no-op: byte-equal targets
//! are not touched at all (F30 lesson: Caseware itself stamps on mere opens).

use crate::error::Result;
use std::path::{Path, PathBuf};

/// Suffix of the staging file while a commit is in flight.
pub const STAGE_SUFFIX: &str = ".~ocw";

fn stage_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_os_string();
    s.push(STAGE_SUFFIX);
    PathBuf::from(s)
}

/// Atomically replace `target` with `bytes`.
///
/// Sequence: write `<target>.~ocw`, flush to disk, rename over `target`
/// (same-volume rename = atomic on NTFS). The staging file is removed on
/// write failure so a killed commit leaves no litter.
pub fn commit_bytes(target: &Path, bytes: &[u8]) -> Result<()> {
    let stage = stage_path(target);
    {
        let mut f = std::fs::File::create(&stage)?;
        use std::io::Write;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    match std::fs::rename(&stage, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&stage);
            Err(e.into())
        }
    }
}

/// Commit only when the bytes actually differ. Returns whether anything was
/// written; a missing target counts as different.
pub fn commit_if_changed(target: &Path, bytes: &[u8]) -> Result<bool> {
    match std::fs::read(target) {
        Ok(cur) if cur == bytes => Ok(false),
        _ => {
            commit_bytes(target, bytes)?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ocw-transact-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn commit_creates_then_replaces() {
        let dir = tmpdir("create");
        let target = dir.join("x.dbf");
        assert!(!target.exists());
        commit_bytes(&target, b"v1").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");
        commit_bytes(&target, b"version-two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"version-two");
        assert!(!dir.join("x.dbf.~ocw").exists(), "stage cleaned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_if_changed_is_a_true_noop_when_equal() {
        let dir = tmpdir("noop");
        let target = dir.join("y.dbf");
        assert_eq!(commit_if_changed(&target, b"abc").unwrap(), true);
        let meta1 = std::fs::metadata(&target).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(commit_if_changed(&target, b"abc").unwrap(), false);
        let meta2 = std::fs::metadata(&target).unwrap().modified().unwrap();
        assert_eq!(meta1, meta2, "equal commit must not touch the file");
        assert_eq!(commit_if_changed(&target, b"abd").unwrap(), true);
        assert_eq!(std::fs::read(&target).unwrap(), b"abd");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

