//! 공급자 API 키 풀 저장소(`secrets/keys.enc`) — 09-key-pool-rotation §4.
//!
//! [`crate::LocalSecretStore`](단일 Z.ai 키)와 같은 마스터 키·같은 봉투
//! 형식(`IYAGI-SECRET-1`)을 쓰고 AEAD 컨텍스트만 다르다. 앱(src-tauri
//! 브리지)이 쓰고 데몬이 요청 시점마다 다시 읽는다 — 추가·삭제가 즉시
//! 반영되고 비밀은 RPC를 통하지 않는다(파일이 진실 원본).
//!
//! 디스크 평문(JSON):
//! `{"version":1,"entries":[{"id","provider","label","secret","created_epoch_ms"}]}`
//!
//! 상한: 항목 32개, 키 8..1024바이트, 파일 4096바이트(기존 LIMIT 재사용).
//! 쓰기는 temp+rename 원자적, 디렉터리 0700·파일 0600(기존 규칙 그대로).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{read_private_limited, write_private_temp, FORMAT};

const POOL_CONTEXT: &[u8] = b"iyagi:provider-key-pool:v1";
const POOL_FILE: &str = "keys.enc";
/// 풀 항목 상한(09 §4). 늘릴 때 `LIMIT` 전체 파일 상한도 같이 본다.
const MAX_ENTRIES: usize = 32;
/// 풀 파일 상한(바이트). 항목 32개의 JSON 봉투가 여유 있게 들어온다.
const POOL_LIMIT: u64 = 16 * 1024;
/// 키 길이 하한 — 실수로 빈/1글자 값을 등록하는 것을 막는다.
const MIN_SECRET: usize = 8;

static POOL_ACCESS: Mutex<()> = Mutex::new(());

/// 목록·상태용 키 정보. **비밀 없음** — UI·로그·RPC에 흘러가는 형태다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolKeyInfo {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub created_epoch_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct PoolEntry {
    id: String,
    provider: String,
    label: String,
    secret: String,
    created_epoch_ms: u64,
}

#[derive(Deserialize)]
struct PoolFile {
    version: u32,
    entries: Vec<PoolEntry>,
}

/// 직렬화 전용 참조 뷰 — 비밀을 담은 항목을 복제하지 않고 쓴다.
#[derive(Serialize)]
struct PoolFileRef<'a> {
    version: u32,
    entries: &'a [PoolEntry],
}

pub struct KeyPoolStore {
    directory: PathBuf,
}

impl KeyPoolStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            directory: data_dir.join("secrets"),
        }
    }

    /// 비밀 없는 항목 목록(등록순).
    pub fn list(&self) -> Result<Vec<PoolKeyInfo>, &'static str> {
        Ok(self
            .read_file()?
            .into_iter()
            .map(|entry| PoolKeyInfo {
                id: entry.id,
                provider: entry.provider,
                label: entry.label,
                created_epoch_ms: entry.created_epoch_ms,
            })
            .collect())
    }

    /// 데몬 전용: 정보 + 비밀을 함께(비밀은 [`Zeroizing`], 드롭 시 지워진다).
    pub fn read_all_zeroizing(
        &self,
    ) -> Result<Vec<(PoolKeyInfo, Zeroizing<String>)>, &'static str> {
        Ok(self
            .read_file()?
            .into_iter()
            .map(entry_with_secret)
            .collect())
    }

    /// 키 추가. 라벨은 비우면 id 앞 8글자로 대체한다. 반환값은 비밀 없는 정보.
    pub fn add(
        &self,
        provider: &str,
        label: &str,
        secret: &str,
    ) -> Result<PoolKeyInfo, &'static str> {
        let _guard = POOL_ACCESS.lock().map_err(|_| "credential_store_error")?;
        if secret.len() < MIN_SECRET || secret.len() > 1024 {
            return Err("api_key_invalid");
        }
        if provider.is_empty() || provider.len() > 32 || label.len() > 64 {
            return Err("api_key_invalid");
        }
        let mut entries = self.read_entries_unlocked()?;
        if entries.len() >= MAX_ENTRIES {
            return Err("key_pool_full");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let label = if label.trim().is_empty() {
            id[..8].to_string()
        } else {
            label.trim().to_string()
        };
        let info = PoolKeyInfo {
            id: id.clone(),
            provider: provider.to_string(),
            label,
            created_epoch_ms: now_epoch_ms(),
        };
        entries.push(PoolEntry {
            id,
            provider: info.provider.clone(),
            label: info.label.clone(),
            secret: secret.to_string(),
            created_epoch_ms: info.created_epoch_ms,
        });
        self.write_entries_unlocked(&entries)?;
        Ok(info)
    }

    /// 키 제거. 존재했으면 true.
    pub fn remove(&self, id: &str) -> Result<bool, &'static str> {
        let _guard = POOL_ACCESS.lock().map_err(|_| "credential_store_error")?;
        let mut entries = self.read_entries_unlocked()?;
        let before = entries.len();
        entries.retain(|entry| entry.id != id);
        let removed = entries.len() != before;
        if removed {
            self.write_entries_unlocked(&entries)?;
        }
        Ok(removed)
    }

    // -- 내부 ------------------------------------------------------------

    fn read_file(&self) -> Result<Vec<PoolEntry>, &'static str> {
        let _guard = POOL_ACCESS.lock().map_err(|_| "credential_store_error")?;
        self.read_entries_unlocked()
    }

    /// 락을 이미 잡은 호출자 전용(`add`/`remove` 경로).
    fn read_entries_unlocked(&self) -> Result<Vec<PoolEntry>, &'static str> {
        let Some(bytes) = read_private_limited(&self.directory.join(POOL_FILE), POOL_LIMIT)? else {
            return Ok(Vec::new());
        };
        if !bytes.starts_with(FORMAT) || bytes.len() < FORMAT.len() + 12 + AES_256_GCM.tag_len() {
            return Err("credential_store_error");
        }
        let master = crate::load_master_key(&self.directory)?;
        let nonce: [u8; 12] = bytes[FORMAT.len()..FORMAT.len() + 12]
            .try_into()
            .map_err(|_| "credential_store_error")?;
        let mut ciphertext = Zeroizing::new(bytes[FORMAT.len() + 12..].to_vec());
        let plain = cipher(&master)?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(POOL_CONTEXT),
                ciphertext.as_mut_slice(),
            )
            .map_err(|_| "credential_store_error")?;
        let file: PoolFile = serde_json::from_slice(plain).map_err(|_| "credential_store_error")?;
        if file.version != 1 {
            return Err("credential_store_error");
        }
        Ok(file.entries)
    }

    fn write_entries_unlocked(&self, entries: &[PoolEntry]) -> Result<(), &'static str> {
        let payload = serde_json::to_vec(&PoolFileRef {
            version: 1,
            entries,
        })
        .map_err(|_| "credential_store_error")?;
        if payload.len() > POOL_LIMIT as usize {
            return Err("credential_store_error");
        }
        fs::create_dir_all(&self.directory).map_err(|_| "credential_store_error")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
                .map_err(|_| "credential_store_error")?;
        }
        crate::ensure_master_key(&self.directory, &self.directory.join(POOL_FILE))?;
        let master = crate::load_master_key(&self.directory)?;
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "credential_store_error")?;
        let mut encrypted = payload;
        cipher(&master)?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(POOL_CONTEXT),
                &mut encrypted,
            )
            .map_err(|_| "credential_store_error")?;
        let mut envelope = FORMAT.to_vec();
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&encrypted);
        // 기존 저장소와 같은 temp+rename 원자적 공개(09 §4).
        let temp = write_private_temp(&self.directory, &envelope)?;
        let result = fs::rename(&temp, self.directory.join(POOL_FILE));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|_| "credential_store_error")
    }
}

fn cipher(key: &[u8; 32]) -> Result<LessSafeKey, &'static str> {
    UnboundKey::new(&AES_256_GCM, key)
        .map(LessSafeKey::new)
        .map_err(|_| "credential_store_error")
}

fn entry_with_secret(entry: PoolEntry) -> (PoolKeyInfo, Zeroizing<String>) {
    (
        PoolKeyInfo {
            id: entry.id,
            provider: entry.provider,
            label: entry.label,
            created_epoch_ms: entry.created_epoch_ms,
        },
        Zeroizing::new(entry.secret),
    )
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "zai-pool-key-aaaa1111";
    const KEY_B: &str = "zai-pool-key-bbbb2222";

    #[test]
    fn empty_pool_lists_and_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyPoolStore::new(dir.path());
        assert!(store.list().unwrap().is_empty());
        assert!(store.read_all_zeroizing().unwrap().is_empty());
        assert!(!store.remove("nope").unwrap());
    }

    #[test]
    fn round_trip_add_list_remove_without_leaking_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyPoolStore::new(dir.path());
        let a = store.add("zai", "개인 키", KEY_A).unwrap();
        let b = store.add("zai", "", KEY_B).unwrap();
        assert_ne!(a.id, b.id);
        // 빈 라벨은 id 접두로 대체된다.
        assert_eq!(b.label, b.id[..8]);

        // 디스크 봉투에 평문 키가 없다.
        let raw = fs::read(dir.path().join("secrets/keys.enc")).unwrap();
        assert!(!raw.windows(KEY_A.len()).any(|w| w == KEY_A.as_bytes()));
        assert!(!raw.windows(KEY_B.len()).any(|w| w == KEY_B.as_bytes()));

        // 목록·재오픈: 비밀 없는 정보와 순서가 보존된다.
        let reopened = KeyPoolStore::new(dir.path());
        let list = reopened.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].label, "개인 키");
        assert!((list[0].provider == "zai") && (list[1].provider == "zai"));
        let secrets = reopened.read_all_zeroizing().unwrap();
        assert_eq!(secrets[0].1.as_str(), KEY_A);
        assert_eq!(secrets[1].1.as_str(), KEY_B);
        assert_eq!(secrets[0].0.label, "개인 키");

        // 제거는 해당 항목만 지운다.
        assert!(reopened.remove(&a.id).unwrap());
        assert_eq!(reopened.list().unwrap().len(), 1);
        assert_eq!(reopened.read_all_zeroizing().unwrap()[0].0.id, b.id);
        assert!(!reopened.remove(&a.id).unwrap());
    }

    /// 풀과 단일 키 저장소는 같은 master.key를 공유하지만 컨텍스트가 달라
    /// 서로의 봉투를 열 수 없다(한쪽 유출이 다른 쪽을 열어주지 않는다).
    #[test]
    fn pool_and_single_key_stores_share_master_but_not_context() {
        let dir = tempfile::tempdir().unwrap();
        crate::LocalSecretStore::new(dir.path())
            .write("single-key-secret")
            .unwrap();
        let pool = KeyPoolStore::new(dir.path());
        pool.add("zai", "pool", KEY_A).unwrap();
        // 각자 자기 것을 연다.
        assert_eq!(
            crate::LocalSecretStore::new(dir.path()).read().unwrap(),
            Some("single-key-secret".to_string())
        );
        assert_eq!(pool.read_all_zeroizing().unwrap()[0].1.as_str(), KEY_A);
        // 마스터 키 파일은 하나다.
        assert!(dir.path().join("secrets/master.key").is_file());
    }

    #[test]
    fn bounds_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyPoolStore::new(dir.path());
        assert_eq!(
            store.add("zai", "x", "short").unwrap_err(),
            "api_key_invalid"
        );
        assert_eq!(store.add("", "x", KEY_A).unwrap_err(), "api_key_invalid");
        assert_eq!(
            store.add("zai", &"l".repeat(65), KEY_A).unwrap_err(),
            "api_key_invalid"
        );
        for n in 0..MAX_ENTRIES {
            store
                .add("zai", &format!("k{n}"), &format!("{KEY_A}-{n}"))
                .unwrap();
        }
        assert_eq!(
            store.add("zai", "one more", KEY_B).unwrap_err(),
            "key_pool_full"
        );
    }
}
