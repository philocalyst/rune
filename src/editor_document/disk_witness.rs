//! Bounded native disk evidence for a future guarded-save transaction.
//!
//! Opt-in owner state only; not wired to public native/Lisp Save. Capture is permitted only at an
//! explicit trusted read/visit boundary, never as a substitute for an unknown
//! baseline at Save. Verification detects changes but is NOT an atomic pathname
//! compare-and-swap: another writer can still race after verification.
//! Unix also compares device/inode identity. Other platforms currently detect
//! content/size changes only; do not claim same-byte replacement detection there.
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

#[derive(Clone, Debug)]
pub(super) struct DiskWitness(DiskState);
#[derive(Clone, Debug)]
enum DiskState {
    Missing,
    Existing {
        bytes: std::sync::Arc<[u8]>,
        identity: Identity,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}
fn conflict(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn identity(metadata: &fs::Metadata) -> Identity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Identity { dev: metadata.dev(), ino: metadata.ino() }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Identity {}
    }
}
fn regular(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => Ok(Some(m)),
        Ok(_) => Err(conflict("Disk witness requires a regular non-symlink file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
impl DiskWitness {
    pub(super) fn utf8(&self) -> io::Result<&str> {
        match &self.0 {
            DiskState::Existing { bytes, .. } => {
                std::str::from_utf8(bytes).map_err(|_| conflict("Visited file is not valid UTF-8"))
            }
            DiskState::Missing => {
                Err(io::Error::new(io::ErrorKind::NotFound, "Visited file does not exist"))
            }
        }
    }
    pub(super) fn from_written(bytes: Vec<u8>, metadata: &fs::Metadata) -> Self {
        Self(DiskState::Existing { bytes: bytes.into(), identity: identity(metadata) })
    }
    pub(super) fn matches_bytes(&self, expected: &[u8]) -> bool {
        matches!(&self.0, DiskState::Existing{bytes,..} if bytes.as_ref()==expected)
    }
    pub(super) fn capture(path: &Path, max_bytes: usize) -> io::Result<Self> {
        let Some(before) = regular(path)? else { return Ok(Self(DiskState::Missing)) };
        if before.len() > max_bytes as u64 {
            return Err(conflict("Disk witness exceeds byte limit"));
        }
        let mut file = fs::File::open(path)?;
        let opened = file.metadata()?;
        if !opened.is_file() || identity(&before) != identity(&opened) {
            return Err(conflict("File changed while opening disk witness"));
        }
        let mut bytes = Vec::new();
        let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX).saturating_add(1);
        (&mut file).take(limit).read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(conflict("Disk witness exceeds byte limit"));
        }
        let witness =
            Self(DiskState::Existing { bytes: bytes.into(), identity: identity(&opened) });
        // Detect a concurrent content/path change during capture. This does not
        // establish historical trust; the caller supplies that boundary.
        witness.verify(path)?;
        Ok(witness)
    }
    pub(super) fn verify(&self, path: &Path) -> io::Result<()> {
        let current = regular(path)?;
        match (&self.0, current) {
            (DiskState::Missing, None) => Ok(()),
            (DiskState::Missing, Some(_)) => {
                Err(conflict("Externally created file conflicts with missing baseline"))
            }
            (DiskState::Existing { .. }, None) => {
                Err(conflict("Visited file was externally removed"))
            }
            (DiskState::Existing { bytes, identity: expected }, Some(before)) => {
                if identity(&before) != *expected || before.len() != bytes.len() as u64 {
                    return Err(conflict("Visited file identity or size changed"));
                }
                let mut file = fs::File::open(path)?;
                let opened = file.metadata()?;
                if !opened.is_file() || identity(&opened) != *expected {
                    return Err(conflict("Visited file changed while opening"));
                }
                let mut offset: usize = 0;
                let mut chunk = [0; 8192];
                loop {
                    let n = file.read(&mut chunk)?;
                    if n == 0 {
                        break;
                    }
                    let end =
                        offset.checked_add(n).ok_or_else(|| conflict("File size overflow"))?;
                    if bytes.get(offset..end) != Some(&chunk[..n]) {
                        return Err(conflict("Visited file content changed externally"));
                    }
                    offset = end;
                }
                if offset != bytes.len() {
                    return Err(conflict("Visited file content was truncated"));
                }
                let after = regular(path)?
                    .ok_or_else(|| conflict("Visited file removed during verification"))?;
                if identity(&after) != *expected || after.len() != bytes.len() as u64 {
                    return Err(conflict("Visited file replaced during verification"));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "rune-witness-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn same_size_reset_mtime_content_change_rejected_without_touching_file() {
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, b"old").unwrap();
        let m = fs::metadata(&p).unwrap().modified().unwrap();
        let w = DiskWitness::capture(&p, 3).unwrap();
        fs::write(&p, b"new").unwrap();
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(m))
            .unwrap();
        assert!(w.verify(&p).is_err());
        assert_eq!(fs::read(&p).unwrap(), b"new");
    }
    #[test]
    fn missing_and_limits_fail_closed() {
        let d = Dir::new();
        let p = d.0.join("file");
        let w = DiskWitness::capture(&p, 0).unwrap();
        w.verify(&p).unwrap();
        fs::write(&p, b"x").unwrap();
        assert!(w.verify(&p).is_err());
        assert!(DiskWitness::capture(&p, 0).is_err());
        let existing = DiskWitness::capture(&p, 1).unwrap();
        existing.verify(&p).unwrap();
        fs::remove_file(&p).unwrap();
        assert!(existing.verify(&p).is_err());
        assert!(DiskWitness::capture(&d.0, 10).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn identical_replacement_and_symlink_rejected() {
        let d = Dir::new();
        let p = d.0.join("file");
        let other = d.0.join("other");
        fs::write(&p, b"same").unwrap();
        let w = DiskWitness::capture(&p, 4).unwrap();
        fs::write(&other, b"same").unwrap();
        fs::rename(&other, &p).unwrap();
        assert!(w.verify(&p).is_err());
        fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink(&other, &p).unwrap();
        assert!(DiskWitness::capture(&p, 4).is_err());
        assert!(w.verify(&p).is_err());
    }
}
