//! OBS password storage stays outside portable JSON configuration.

use std::sync::{Arc, Mutex};

use thiserror::Error;

const SERVICE: &str = "com.snenk.snenkbot.obs";
const ACCOUNT: &str = "websocket-password";

#[derive(Clone, Copy, Debug, Error)]
#[error("the system credential manager could not access the OBS password")]
pub struct ObsCredentialError;

pub trait ObsCredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<String>, ObsCredentialError>;
    fn save(&self, password: &str) -> Result<(), ObsCredentialError>;
    fn clear(&self) -> Result<(), ObsCredentialError>;
}

/// Reads the system credential once at startup. Settings writes update the
/// in-memory value only after the credential manager confirms persistence.
pub struct CachedObsCredentialStore {
    backend: Arc<dyn ObsCredentialStore>,
    cached: Mutex<Result<Option<String>, ObsCredentialError>>,
}

impl CachedObsCredentialStore {
    pub fn new(backend: Arc<dyn ObsCredentialStore>) -> Self {
        let cached = backend.load();
        Self {
            backend,
            cached: Mutex::new(cached),
        }
    }

    pub fn without_preload(backend: Arc<dyn ObsCredentialStore>) -> Self {
        Self {
            backend,
            cached: Mutex::new(Ok(None)),
        }
    }
}

impl ObsCredentialStore for CachedObsCredentialStore {
    fn load(&self) -> Result<Option<String>, ObsCredentialError> {
        self.cached
            .lock()
            .expect("OBS credential cache lock poisoned")
            .clone()
    }

    fn save(&self, password: &str) -> Result<(), ObsCredentialError> {
        let mut cached = self
            .cached
            .lock()
            .expect("OBS credential cache lock poisoned");
        self.backend.save(password)?;
        *cached = Ok(Some(password.to_owned()));
        Ok(())
    }

    fn clear(&self) -> Result<(), ObsCredentialError> {
        let mut cached = self
            .cached
            .lock()
            .expect("OBS credential cache lock poisoned");
        self.backend.clear()?;
        *cached = Ok(None);
        Ok(())
    }
}

pub struct SystemObsCredentialStore;

impl SystemObsCredentialStore {
    fn entry() -> Result<keyring::Entry, ObsCredentialError> {
        keyring::Entry::new(SERVICE, ACCOUNT).map_err(|_| ObsCredentialError)
    }
}

impl ObsCredentialStore for SystemObsCredentialStore {
    fn load(&self) -> Result<Option<String>, ObsCredentialError> {
        match Self::entry()?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(ObsCredentialError),
        }
    }

    fn save(&self, password: &str) -> Result<(), ObsCredentialError> {
        Self::entry()?
            .set_password(password)
            .map_err(|_| ObsCredentialError)
    }

    fn clear(&self) -> Result<(), ObsCredentialError> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(ObsCredentialError),
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

    impl ObsCredentialStore for MemoryStore {
        fn load(&self) -> Result<Option<String>, ObsCredentialError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(ObsCredentialError);
            }
            Ok(self.value.lock().unwrap().clone())
        }

        fn save(&self, password: &str) -> Result<(), ObsCredentialError> {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(ObsCredentialError);
            }
            *self.value.lock().unwrap() = Some(password.into());
            Ok(())
        }

        fn clear(&self) -> Result<(), ObsCredentialError> {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(ObsCredentialError);
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn startup_result_is_shared_without_more_backend_reads() {
        let backend = Arc::new(MemoryStore {
            value: Mutex::new(Some("initial".into())),
            reads: AtomicUsize::new(0),
            fail_reads: AtomicBool::new(false),
            fail_writes: AtomicBool::new(false),
        });
        let cache = Arc::new(CachedObsCredentialStore::new(backend.clone()));
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
    fn failed_startup_and_writes_keep_the_cached_result() {
        let backend = Arc::new(MemoryStore {
            value: Mutex::new(None),
            reads: AtomicUsize::new(0),
            fail_reads: AtomicBool::new(true),
            fail_writes: AtomicBool::new(true),
        });
        let cache = CachedObsCredentialStore::new(backend.clone());
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
        let cache = CachedObsCredentialStore::without_preload(backend.clone());
        assert!(cache.load().unwrap().is_none());
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
        cache.save("new").unwrap();
        assert_eq!(cache.load().unwrap().as_deref(), Some("new"));
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
    }
}
