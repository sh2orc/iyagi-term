use iyagi_termd_lib::connections::CredentialStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

#[derive(Default)]
pub struct MemoryCredentials(Mutex<HashMap<String, Zeroizing<String>>>);
impl MemoryCredentials {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}
impl CredentialStore for MemoryCredentials {
    fn put(&self, account: &str, value: &str) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(account.into(), Zeroizing::new(value.into()));
        Ok(())
    }
    fn get(&self, account: &str) -> std::io::Result<Zeroizing<String>> {
        self.0.lock().unwrap().get(account).cloned().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "credential missing")
        })
    }
    fn delete(&self, account: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}
