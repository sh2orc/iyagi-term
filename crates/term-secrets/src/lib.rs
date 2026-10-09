//! Local authenticated encryption. This store never calls an OS keychain.
//! The random key and ciphertext live in owner-only files, outside settings.
//!
//! Shared by the Tauri shell (Settings → Z.ai Coding Plan writes the key) and
//! by `iyagi-termd` (resolves it at launch time for `claude_provider`
//! routing). Both sides open the same `<data_dir>/secrets/` directory; the
//! on-disk format (`FORMAT`/`CONTEXT`/`LIMIT`) is frozen so a `zai.enc`
//! written by an older app build still decrypts here.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

const FORMAT: &[u8] = b"IYAGI-SECRET-1\0";
const CONTEXT: &[u8] = b"iyagi:zai-coding-plan-api-key:v1";
const LIMIT: u64 = 4096;
static ACCESS: Mutex<()> = Mutex::new(());

pub struct LocalSecretStore {
    directory: PathBuf,
}

impl LocalSecretStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            directory: data_dir.join("secrets"),
        }
    }

    pub fn read(&self) -> Result<Option<String>, &'static str> {
        let _guard = ACCESS.lock().map_err(|_| "credential_store_error")?;
        let Some(bytes) = read_private(&self.directory.join("zai.enc"))? else {
            return Ok(None);
        };
        let key = self.load_key()?;
        if !bytes.starts_with(FORMAT) || bytes.len() < FORMAT.len() + 12 + AES_256_GCM.tag_len() {
            return Err("credential_store_error");
        }
        let nonce: [u8; 12] = bytes[FORMAT.len()..FORMAT.len() + 12]
            .try_into()
            .map_err(|_| "credential_store_error")?;
        // The in-place open turns this buffer into plaintext; wipe it on drop
        // so the only surviving copy is the one handed to the caller.
        let mut ciphertext = Zeroizing::new(bytes[FORMAT.len() + 12..].to_vec());
        let plain = cipher(&key)?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(CONTEXT),
                ciphertext.as_mut_slice(),
            )
            .map_err(|_| "credential_store_error")?;
        let secret = String::from_utf8(plain.to_vec()).map_err(|_| "credential_store_error")?;
        Ok(Some(secret))
    }

    /// [`Self::read`] with the plaintext wrapped in [`Zeroizing`] so it is
    /// wiped from memory when the caller drops it. The daemon's launch path
    /// uses this: the token must only ever live in the child's env map.
    pub fn read_zeroizing(&self) -> Result<Option<Zeroizing<String>>, &'static str> {
        // `String` moves by pointer, so wrapping here leaves no stray copy.
        self.read().map(|secret| secret.map(Zeroizing::new))
    }

    pub fn write(&self, secret: &str) -> Result<(), &'static str> {
        let _guard = ACCESS.lock().map_err(|_| "credential_store_error")?;
        if secret.is_empty() || secret.len() > 1024 {
            return Err("api_key_invalid");
        }
        fs::create_dir_all(&self.directory).map_err(|_| "credential_store_error")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
                .map_err(|_| "credential_store_error")?;
        }
        let key_path = self.directory.join("master.key");
        // Never generate a replacement key over an existing encrypted secret.
        // Missing/corrupt key material must leave that secret untouched.
        if !key_path.exists() {
            if self.directory.join("zai.enc").exists() {
                return Err("credential_store_error");
            }
            let mut key = [0u8; 32];
            SystemRandom::new()
                .fill(&mut key)
                .map_err(|_| "credential_store_error")?;
            let temp = self.write_temp(&key)?;
            // Publish a complete key without overwriting a concurrent creator.
            let result = fs::hard_link(&temp, &key_path);
            let _ = fs::remove_file(&temp);
            if let Err(error) = result {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err("credential_store_error");
                }
            }
        }
        let key = self.load_key()?;
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "credential_store_error")?;
        let mut encrypted = secret.as_bytes().to_vec();
        cipher(&key)?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(CONTEXT),
                &mut encrypted,
            )
            .map_err(|_| "credential_store_error")?;
        let mut envelope = FORMAT.to_vec();
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&encrypted);
        let temp = self.write_temp(&envelope)?;
        let result = fs::rename(&temp, self.directory.join("zai.enc"));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|_| "credential_store_error")
    }

    pub fn remove(&self) -> Result<(), &'static str> {
        let _guard = ACCESS.lock().map_err(|_| "credential_store_error")?;
        match fs::remove_file(self.directory.join("zai.enc")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("credential_store_error"),
        }
    }

    fn load_key(&self) -> Result<[u8; 32], &'static str> {
        read_private(&self.directory.join("master.key"))?
            .ok_or("credential_store_error")?
            .try_into()
            .map_err(|_| "credential_store_error")
    }

    fn write_temp(&self, bytes: &[u8]) -> Result<PathBuf, &'static str> {
        let path = self
            .directory
            .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        result
            .map(|()| path)
            .map_err(|_: std::io::Error| "credential_store_error")
    }
}

fn cipher(key: &[u8; 32]) -> Result<LessSafeKey, &'static str> {
    UnboundKey::new(&AES_256_GCM, key)
        .map(LessSafeKey::new)
        .map_err(|_| "credential_store_error")
}

fn read_private(path: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("credential_store_error"),
    };
    let metadata = file.metadata().map_err(|_| "credential_store_error")?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err("credential_store_error");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("credential_store_error");
        }
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "credential_store_error")?;
    if bytes.len() > LIMIT as usize {
        return Err("credential_store_error");
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_status_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalSecretStore::new(dir.path());
        assert_eq!(store.read().unwrap(), None);
        assert!(store.read_zeroizing().unwrap().is_none());
        store.remove().unwrap();
        assert!(!dir.path().join("secrets").exists());
    }

    #[test]
    fn round_trip_restart_rotate_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalSecretStore::new(dir.path());
        store.write("test-zai-secret-one").unwrap();
        let bytes = fs::read(store.directory.join("zai.enc")).unwrap();
        assert!(!bytes.windows(19).any(|part| part == b"test-zai-secret-one"));
        let reopened = LocalSecretStore::new(dir.path());
        assert_eq!(
            reopened.read().unwrap().as_deref(),
            Some("test-zai-secret-one")
        );
        reopened.write("test-zai-secret-two").unwrap();
        assert_eq!(
            store.read().unwrap().as_deref(),
            Some("test-zai-secret-two")
        );
        assert_ne!(bytes, fs::read(store.directory.join("zai.enc")).unwrap());
        store.remove().unwrap();
        assert_eq!(reopened.read().unwrap(), None);
    }

    /// `read_zeroizing`은 `read`와 같은 평문을 돌려주되 drop 시 지워지는
    /// 래퍼로 감싼다 — 데몬 실행 경로가 쓰는 API다.
    #[test]
    fn read_zeroizing_matches_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalSecretStore::new(dir.path());
        store.write("test-zai-secret-zeroizing").unwrap();
        let secret = store.read_zeroizing().unwrap().expect("secret present");
        assert_eq!(secret.as_str(), "test-zai-secret-zeroizing");
        assert_eq!(store.read().unwrap().as_deref(), Some(secret.as_str()));
        store.remove().unwrap();
        assert!(store.read_zeroizing().unwrap().is_none());
    }

    #[test]
    fn corruption_is_rejected_and_missing_master_key_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalSecretStore::new(dir.path());
        store.write("test-zai-secret").unwrap();
        let path = store.directory.join("zai.enc");
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(store.read().is_err());
        assert!(store.read_zeroizing().is_err());
        fs::remove_file(store.directory.join("master.key")).unwrap();
        assert!(store.write("replacement-secret").is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn private_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = LocalSecretStore::new(dir.path());
        store.write("test-zai-secret").unwrap();
        assert_eq!(
            fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["master.key", "zai.enc"] {
            assert_eq!(
                fs::metadata(store.directory.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::set_permissions(
            store.directory.join("master.key"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(store.read().is_err());
    }
}
