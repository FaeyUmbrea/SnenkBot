//! VTube Studio plugin authorization stays outside portable configuration files.

use std::sync::{Arc, Mutex};

use thiserror::Error;

const SERVICE: &str = "com.snenk.snenkbot.vtube-studio";
const ACCOUNT: &str = "plugin-token";

#[derive(Clone, Copy, Debug, Error)]
#[error("the system credential manager could not access the VTube Studio authorization")]
pub struct VtubeCredentialError;

pub trait VtubeCredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<String>, VtubeCredentialError>;
    fn save(&self, token: &str) -> Result<(), VtubeCredentialError>;
    fn clear(&self) -> Result<(), VtubeCredentialError>;
}

/// Reads the system credential once at startup. Explicit authorization writes
/// update the cached token only after durable storage succeeds.
pub struct CachedVtubeCredentialStore {
    backend: Arc<dyn VtubeCredentialStore>,
    cached: Mutex<Result<Option<String>, VtubeCredentialError>>,
}

impl CachedVtubeCredentialStore {
    pub fn new(backend: Arc<dyn VtubeCredentialStore>) -> Self {
        let cached = backend.load();
        Self {
            backend,
            cached: Mutex::new(cached),
        }
    }

    pub fn without_preload(backend: Arc<dyn VtubeCredentialStore>) -> Self {
        Self {
            backend,
            cached: Mutex::new(Ok(None)),
        }
    }
}

impl VtubeCredentialStore for CachedVtubeCredentialStore {
    fn load(&self) -> Result<Option<String>, VtubeCredentialError> {
        self.cached
            .lock()
            .expect("VTube Studio credential cache lock poisoned")
            .clone()
    }

    fn save(&self, token: &str) -> Result<(), VtubeCredentialError> {
        let mut cached = self
            .cached
            .lock()
            .expect("VTube Studio credential cache lock poisoned");
        self.backend.save(token)?;
        *cached = Ok(Some(token.to_owned()));
        Ok(())
    }

    fn clear(&self) -> Result<(), VtubeCredentialError> {
        let mut cached = self
            .cached
            .lock()
            .expect("VTube Studio credential cache lock poisoned");
        self.backend.clear()?;
        *cached = Ok(None);
        Ok(())
    }
}

pub struct SystemVtubeCredentialStore;

impl SystemVtubeCredentialStore {
    fn entry() -> Result<keyring::Entry, VtubeCredentialError> {
        keyring::Entry::new(SERVICE, ACCOUNT).map_err(|_| VtubeCredentialError)
    }
}

impl VtubeCredentialStore for SystemVtubeCredentialStore {
    fn load(&self) -> Result<Option<String>, VtubeCredentialError> {
        match Self::entry()?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(VtubeCredentialError),
        }
    }

    fn save(&self, token: &str) -> Result<(), VtubeCredentialError> {
        Self::entry()?
            .set_password(token)
            .map_err(|_| VtubeCredentialError)
    }

    fn clear(&self) -> Result<(), VtubeCredentialError> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(VtubeCredentialError),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;

    use super::*;

    struct MemoryStore {
        value: Mutex<Option<String>>,
        reads: AtomicUsize,
        fail_reads: AtomicBool,
        fail_writes: AtomicBool,
    }

    impl VtubeCredentialStore for MemoryStore {
        fn load(&self) -> Result<Option<String>, VtubeCredentialError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(VtubeCredentialError);
            }
            Ok(self.value.lock().unwrap().clone())
        }

        fn save(&self, token: &str) -> Result<(), VtubeCredentialError> {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(VtubeCredentialError);
            }
            *self.value.lock().unwrap() = Some(token.into());
            Ok(())
        }

        fn clear(&self) -> Result<(), VtubeCredentialError> {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(VtubeCredentialError);
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn startup_result_is_shared_and_updates_after_persistence() {
        let backend = Arc::new(MemoryStore {
            value: Mutex::new(Some("initial".into())),
            reads: AtomicUsize::new(0),
            fail_reads: AtomicBool::new(false),
            fail_writes: AtomicBool::new(false),
        });
        let cache = Arc::new(CachedVtubeCredentialStore::new(backend.clone()));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                thread::spawn(move || assert_eq!(cache.load().unwrap().as_deref(), Some("initial")))
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
        cache.save("updated").unwrap();
        assert_eq!(cache.load().unwrap().as_deref(), Some("updated"));
        cache.clear().unwrap();
        assert!(cache.load().unwrap().is_none());
        assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_startup_read_remains_cached_until_explicit_write() {
        let backend = Arc::new(MemoryStore {
            value: Mutex::new(None),
            reads: AtomicUsize::new(0),
            fail_reads: AtomicBool::new(true),
            fail_writes: AtomicBool::new(true),
        });
        let cache = CachedVtubeCredentialStore::new(backend.clone());
        backend.fail_reads.store(false, Ordering::SeqCst);
        assert!(cache.load().is_err());
        assert!(cache.load().is_err());
        assert!(cache.save("new").is_err());
        assert!(cache.load().is_err());
        backend.fail_writes.store(false, Ordering::SeqCst);
        cache.save("new").unwrap();
        assert_eq!(cache.load().unwrap().as_deref(), Some("new"));
        assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn absent_configuration_skips_backend_read_and_explicit_save_populates_cache() {
        let backend = Arc::new(MemoryStore {
            value: Mutex::new(Some("old".into())),
            reads: AtomicUsize::new(0),
            fail_reads: AtomicBool::new(false),
            fail_writes: AtomicBool::new(false),
        });
        let cache = CachedVtubeCredentialStore::without_preload(backend.clone());
        assert!(cache.load().unwrap().is_none());
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
        cache.save("new").unwrap();
        assert_eq!(cache.load().unwrap().as_deref(), Some("new"));
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
    }
}
