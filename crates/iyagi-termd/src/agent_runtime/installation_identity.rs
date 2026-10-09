//! Content identity of a configured CLI entry point. This does not claim to
//! pin an interpreter, a launcher's child binary, or a loaded process image.
use super::installation::ProbeFailure;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, Metadata, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::time::Instant;

pub const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableIdentity {
    pub canonical_path: String,
    pub bytes: u64,
    pub sha256: String,
}

impl ExecutableIdentity {
    pub fn is_valid(&self) -> bool {
        Path::new(&self.canonical_path).is_absolute()
            && self.bytes <= MAX_EXECUTABLE_BYTES
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    pub fn capture(program: &str, deadline: Instant) -> Result<Self, ProbeFailure> {
        let path = Path::new(program);
        if !path.is_absolute() {
            return Err(ProbeFailure::Failed);
        }
        let canonical = fs::canonicalize(path).map_err(map_io)?;
        let canonical_path = canonical.to_str().ok_or(ProbeFailure::Failed)?.to_owned();
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Opening a FIFO must not block before the regular-file check.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut file = options.open(&canonical).map_err(map_io)?;
        let before = file.metadata().map_err(map_io)?;
        if !before.is_file() || before.len() > MAX_EXECUTABLE_BYTES {
            return Err(ProbeFailure::Failed);
        }
        let mut digest = Sha256::new();
        let mut read = 0u64;
        let mut chunk = [0u8; 64 * 1024];
        loop {
            if Instant::now() >= deadline {
                return Err(ProbeFailure::TimedOut);
            }
            let n = file.read(&mut chunk).map_err(map_io)?;
            if n == 0 {
                break;
            }
            read += n as u64;
            if read > before.len() || read > MAX_EXECUTABLE_BYTES {
                return Err(ProbeFailure::Changed);
            }
            digest.update(&chunk[..n]);
        }
        if read != before.len()
            || !same_file(&before, &file.metadata().map_err(map_io)?)
            || canonical != fs::canonicalize(path).map_err(map_io)?
            || !same_file(&before, &fs::metadata(path).map_err(map_io)?)
        {
            return Err(ProbeFailure::Changed);
        }
        Ok(Self {
            canonical_path,
            bytes: read,
            sha256: format!("{:x}", digest.finalize()),
        })
    }
}

fn map_io(error: std::io::Error) -> ProbeFailure {
    if error.kind() == std::io::ErrorKind::NotFound {
        ProbeFailure::NotFound
    } else {
        ProbeFailure::Failed
    }
}

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    if a.len() != b.len() || a.modified().ok() != b.modified().ok() || !b.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.mode() == b.mode()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        a.created().ok() == b.created().ok()
            && a.permissions().readonly() == b.permissions().readonly()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn capture(path: &Path) -> Result<ExecutableIdentity, ProbeFailure> {
        ExecutableIdentity::capture(
            path.to_str().unwrap(),
            Instant::now() + Duration::from_secs(3),
        )
    }

    #[test]
    fn changed_bytes_are_detected_even_with_the_same_length_and_version_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cli");
        fs::write(&file, b"version 1.2.3; first").unwrap();
        let first = capture(&file).unwrap();
        assert!(first.is_valid());
        assert_eq!(capture(&file).unwrap(), first);
        fs::write(&file, b"version 1.2.3; other").unwrap();
        let second = capture(&file).unwrap();
        assert_eq!(first.bytes, second.bytes);
        assert_ne!(first.sha256, second.sha256);
        assert_eq!(first.canonical_path, second.canonical_path);
    }

    #[test]
    fn directories_oversized_files_relative_paths_and_expired_deadlines_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(capture(dir.path()), Err(ProbeFailure::Failed));
        let file = dir.path().join("large");
        fs::File::create(&file)
            .unwrap()
            .set_len(MAX_EXECUTABLE_BYTES + 1)
            .unwrap();
        assert_eq!(capture(&file), Err(ProbeFailure::Failed));
        assert_eq!(capture(Path::new("relative")), Err(ProbeFailure::Failed));
        fs::write(&file, b"small").unwrap();
        assert_eq!(
            ExecutableIdentity::capture(file.to_str().unwrap(), Instant::now()),
            Err(ProbeFailure::TimedOut)
        );
    }

    #[test]
    #[cfg(unix)]
    fn symlink_retargeting_and_special_files_cannot_inherit_an_entrypoint_identity() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        let link = dir.path().join("cli");
        fs::write(&first, b"identical").unwrap();
        fs::write(&second, b"identical").unwrap();
        symlink(&first, &link).unwrap();
        let before = capture(&link).unwrap();
        fs::remove_file(&link).unwrap();
        symlink(&second, &link).unwrap();
        assert_ne!(capture(&link).unwrap(), before);
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(capture(&fifo), Err(ProbeFailure::Failed));
    }
}
