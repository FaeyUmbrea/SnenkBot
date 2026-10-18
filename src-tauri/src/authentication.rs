//! Desktop sign-in identities expose progress without exporting OAuth credentials.

use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

use snenk_bot::runtime::RuntimeSpawner;
use snenk_bot::twitch::credentials::{CredentialBackend, TwitchRole};
use snenk_bot::twitch::device::FormTransport;
use snenk_bot::twitch::login::{LoginEvent, TwitchLogin};

const ACTIVATION_URL: &str = "https://www.twitch.tv/activate";
const SIGN_IN_FAILED: &str = "Twitch sign-in failed. Please try again.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct TwitchAuthentication {
    pub attempt_id: String,
    pub role: TwitchRole,
    pub phase: TwitchAuthenticationPhase,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum TwitchAuthenticationPhase {
    Starting,
    Code { url: String, code: String },
    Review { login: String, user_id: String },
    Saving,
    Connected { login: String },
    Failed { message: String },
    Cancelled,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct StartTwitchLogin {
    pub role: TwitchRole,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ApproveTwitchLogin {
    pub attempt_id: String,
    pub user_id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationIdentity {
    pub attempt_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationErrorCode {
    StaleAttempt,
    NotReady,
    Busy,
    Unavailable,
}

#[derive(Debug, Serialize, Type)]
pub struct AuthenticationError {
    pub code: AuthenticationErrorCode,
    pub message: String,
}

impl AuthenticationError {
    pub(crate) fn new(code: AuthenticationErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self::new(
            AuthenticationErrorCode::Unavailable,
            "Twitch sign-in is unavailable. Please try again.",
        )
    }

    fn stale() -> Self {
        Self::new(
            AuthenticationErrorCode::StaleAttempt,
            "This Twitch sign-in has been replaced. Use the current sign-in.",
        )
    }

    fn busy() -> Self {
        Self::new(
            AuthenticationErrorCode::Busy,
            "Your Twitch account is being saved. Please wait.",
        )
    }

    fn not_ready() -> Self {
        Self::new(
            AuthenticationErrorCode::NotReady,
            "This Twitch sign-in is not ready for that action.",
        )
    }
}

struct Attempt {
    core_id: u64,
    snapshot: TwitchAuthentication,
    publication_failed: bool,
}

#[derive(Default)]
struct State {
    active: Option<Attempt>,
}

type AuthenticationSink = dyn Fn(TwitchAuthentication) -> Result<(), String> + Send + Sync;

pub struct DesktopAuthentication {
    operations: Mutex<()>,
    state: Mutex<State>,
    sink: Box<AuthenticationSink>,
}

impl DesktopAuthentication {
    /// The sink must never call back into this coordinator. It runs under the state
    /// lock so a terminal callback cannot publish before an earlier progress update.
    /// Commands take operations first, then state; callbacks take state only. Never
    /// hold state while calling core methods, which may report progress synchronously.
    pub fn new(
        sink: impl Fn(TwitchAuthentication) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            operations: Mutex::new(()),
            state: Mutex::new(State::default()),
            sink: Box::new(sink),
        }
    }

    pub fn snapshot(&self) -> Result<Option<TwitchAuthentication>, AuthenticationError> {
        Ok(self
            .lock_state()?
            .active
            .as_ref()
            .map(|attempt| attempt.snapshot.clone()))
    }

    pub fn start<B, T>(
        self: &Arc<Self>,
        login: &TwitchLogin<B, T>,
        runtime: &RuntimeSpawner,
        role: TwitchRole,
    ) -> Result<TwitchAuthentication, AuthenticationError>
    where
        B: CredentialBackend + 'static,
        T: FormTransport + Send + Sync + 'static,
    {
        let _operation = self.lock_operations()?;
        self.ensure_not_saving()?;
        let public_id = Uuid::new_v4().to_string();
        let report_id = public_id.clone();
        let coordinator = Arc::clone(self);
        let result = login.start(runtime, role, move |id, event| {
            coordinator.report(&report_id, role, id, event);
        });
        if result.is_err() {
            // Starting is synchronous and establishes the new identity only after
            // core accepted replacement. A rejection before it preserves the old UI.
            let mut state = self.lock_state()?;
            if let Some(attempt) = state.active.as_mut()
                && attempt.snapshot.attempt_id == public_id
            {
                attempt.snapshot.phase = TwitchAuthenticationPhase::Failed {
                    message: SIGN_IN_FAILED.into(),
                };
                self.publish(attempt);
            }
            tracing::error!("Twitch sign-in could not be scheduled");
            return Err(AuthenticationError::unavailable());
        }
        let state = self.lock_state()?;
        let attempt = state
            .active
            .as_ref()
            .ok_or_else(AuthenticationError::unavailable)?;
        if attempt.publication_failed {
            return Err(AuthenticationError::unavailable());
        }
        Ok(attempt.snapshot.clone())
    }

    pub fn approve<B, T>(
        self: &Arc<Self>,
        login: &TwitchLogin<B, T>,
        runtime: &RuntimeSpawner,
        request: ApproveTwitchLogin,
    ) -> Result<(), AuthenticationError>
    where
        B: CredentialBackend + 'static,
        T: FormTransport + Send + Sync + 'static,
    {
        let _operation = self.lock_operations()?;
        let (core_id, role) = {
            let mut state = self.lock_state()?;
            let attempt = current_attempt(&mut state, &request.attempt_id)?;
            match &attempt.snapshot.phase {
                TwitchAuthenticationPhase::Saving => return Err(AuthenticationError::busy()),
                TwitchAuthenticationPhase::Review { user_id, .. }
                    if user_id == &request.user_id => {}
                TwitchAuthenticationPhase::Review { .. } => {
                    return Err(AuthenticationError::stale());
                }
                _ => return Err(AuthenticationError::not_ready()),
            }
            attempt.snapshot.phase = TwitchAuthenticationPhase::Saving;
            self.publish(attempt);
            (attempt.core_id, attempt.snapshot.role)
        };
        let coordinator = Arc::clone(self);
        let report_id = request.attempt_id.clone();
        let result = login.approve_attempt(runtime, core_id, &request.user_id, move |id, event| {
            coordinator.report(&report_id, role, id, event);
        });
        let mut state = self.lock_state()?;
        let attempt = current_attempt(&mut state, &request.attempt_id)?;
        if result.is_err() {
            attempt.snapshot.phase = TwitchAuthenticationPhase::Failed {
                message: SIGN_IN_FAILED.into(),
            };
            self.publish(attempt);
            tracing::error!("Twitch account approval could not be scheduled");
            return Err(AuthenticationError::unavailable());
        }
        if attempt.publication_failed {
            return Err(AuthenticationError::unavailable());
        }
        Ok(())
    }

    pub fn cancel<B, T>(
        &self,
        login: &TwitchLogin<B, T>,
        request: AuthenticationIdentity,
    ) -> Result<(), AuthenticationError>
    where
        B: CredentialBackend + 'static,
        T: FormTransport + Send + Sync + 'static,
    {
        let _operation = self.lock_operations()?;
        let core_id = {
            let mut state = self.lock_state()?;
            let attempt = current_attempt(&mut state, &request.attempt_id)?;
            match attempt.snapshot.phase {
                TwitchAuthenticationPhase::Saving => return Err(AuthenticationError::busy()),
                TwitchAuthenticationPhase::Starting
                | TwitchAuthenticationPhase::Code { .. }
                | TwitchAuthenticationPhase::Review { .. } => {}
                _ => return Err(AuthenticationError::not_ready()),
            }
            attempt.core_id
        };
        if !login.cancel_attempt(core_id) {
            return Err(AuthenticationError::not_ready());
        }
        let mut state = self.lock_state()?;
        let attempt = current_attempt(&mut state, &request.attempt_id)?;
        attempt.snapshot.phase = TwitchAuthenticationPhase::Cancelled;
        self.publish(attempt);
        if attempt.publication_failed {
            return Err(AuthenticationError::unavailable());
        }
        Ok(())
    }

    pub fn verification_url(
        &self,
        request: AuthenticationIdentity,
    ) -> Result<String, AuthenticationError> {
        let mut state = self.lock_state()?;
        let attempt = current_attempt(&mut state, &request.attempt_id)?;
        match &attempt.snapshot.phase {
            TwitchAuthenticationPhase::Code { url, .. } if url == ACTIVATION_URL => Ok(url.clone()),
            _ => Err(AuthenticationError::not_ready()),
        }
    }

    fn ensure_not_saving(&self) -> Result<(), AuthenticationError> {
        if self.lock_state()?.active.as_ref().is_some_and(|attempt| {
            matches!(attempt.snapshot.phase, TwitchAuthenticationPhase::Saving)
        }) {
            return Err(AuthenticationError::busy());
        }
        Ok(())
    }

    fn report(&self, public_id: &str, role: TwitchRole, core_id: u64, event: LoginEvent) {
        let Ok(mut state) = self.lock_state() else {
            tracing::error!("Twitch authentication state lock poisoned");
            return;
        };
        // Core emits Starting exactly once, synchronously inside serialized start.
        // All later callbacks must match both identities before changing state.
        if matches!(event, LoginEvent::Starting) {
            state.active = Some(Attempt {
                core_id,
                snapshot: TwitchAuthentication {
                    attempt_id: public_id.into(),
                    role,
                    phase: TwitchAuthenticationPhase::Starting,
                },
                publication_failed: false,
            });
        }
        let Some(attempt) = state.active.as_mut().filter(|attempt| {
            attempt.core_id == core_id && attempt.snapshot.attempt_id == public_id
        }) else {
            return;
        };
        let phase = match (&attempt.snapshot.phase, event) {
            (TwitchAuthenticationPhase::Starting, LoginEvent::Starting) => {
                TwitchAuthenticationPhase::Starting
            }
            (TwitchAuthenticationPhase::Starting, LoginEvent::Code { url, code })
                if url == ACTIVATION_URL =>
            {
                TwitchAuthenticationPhase::Code { url, code }
            }
            (TwitchAuthenticationPhase::Starting, LoginEvent::Code { .. }) => {
                TwitchAuthenticationPhase::Failed {
                    message: SIGN_IN_FAILED.into(),
                }
            }
            (TwitchAuthenticationPhase::Code { .. }, LoginEvent::Review { login, user_id }) => {
                TwitchAuthenticationPhase::Review { login, user_id }
            }
            (
                TwitchAuthenticationPhase::Saving,
                LoginEvent::Connected {
                    role: connected_role,
                    login,
                },
            ) if connected_role == role => TwitchAuthenticationPhase::Connected { login },
            (
                TwitchAuthenticationPhase::Starting
                | TwitchAuthenticationPhase::Code { .. }
                | TwitchAuthenticationPhase::Review { .. }
                | TwitchAuthenticationPhase::Saving,
                LoginEvent::Failed(_),
            ) => TwitchAuthenticationPhase::Failed {
                message: SIGN_IN_FAILED.into(),
            },
            _ => return,
        };
        attempt.snapshot.phase = phase;
        self.publish(attempt);
    }

    fn publish(&self, attempt: &mut Attempt) {
        attempt.publication_failed = (self.sink)(attempt.snapshot.clone()).is_err();
        if attempt.publication_failed {
            tracing::error!("Twitch authentication update publication failed");
        }
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>, AuthenticationError> {
        self.state
            .lock()
            .map_err(|_| AuthenticationError::unavailable())
    }

    fn lock_operations(&self) -> Result<MutexGuard<'_, ()>, AuthenticationError> {
        self.operations
            .lock()
            .map_err(|_| AuthenticationError::unavailable())
    }
}

fn current_attempt<'a>(
    state: &'a mut State,
    public_id: &str,
) -> Result<&'a mut Attempt, AuthenticationError> {
    state
        .active
        .as_mut()
        .filter(|attempt| attempt.snapshot.attempt_id == public_id)
        .ok_or_else(AuthenticationError::stale)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use snenk_bot::runtime::AppRuntime;
    use snenk_bot::storage::ConfigStore;
    use snenk_bot::twitch::accounts::TwitchAccounts;
    use snenk_bot::twitch::credentials::{CredentialError, TwitchCredentialStore};
    use snenk_bot::twitch::device::{CLIENT_ID, DeviceAuth, DeviceError, FormResponse};

    #[derive(Clone, Default)]
    struct MemoryCredentials(Arc<Mutex<HashMap<String, String>>>);

    impl CredentialBackend for MemoryCredentials {
        fn get(&self, _: &str, account: &str) -> Result<Option<String>, CredentialError> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn set(&self, _: &str, account: &str, value: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
        fn delete(&self, _: &str, account: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    struct FakeTwitch;

    impl FormTransport for FakeTwitch {
        async fn post_form(
            &self,
            url: &str,
            _: &[(&str, &str)],
        ) -> Result<FormResponse, DeviceError> {
            let body = if url.ends_with("/device") {
                r#"{"device_code":"private-device","user_code":"ABCDEFGH","verification_uri":"https://www.twitch.tv/activate","interval":1,"expires_in":30}"#
            } else {
                r#"{"access_token":"private-access","refresh_token":"private-refresh","expires_in":3600,"scope":["user:write:chat"],"token_type":"bearer"}"#
            };
            Ok(FormResponse {
                status: 200,
                body: body.as_bytes().to_vec(),
                retry_after: None,
            })
        }
        async fn get_bearer(&self, _: &str, _: &str) -> Result<FormResponse, DeviceError> {
            Ok(FormResponse {
                status: 200,
                body: format!(r#"{{"client_id":"{CLIENT_ID}","user_id":"456","login":"streamerbot","scopes":["user:write:chat"],"expires_in":3600}}"#).into_bytes(),
                retry_after: None,
            })
        }
    }

    fn login(
        directory: &tempfile::TempDir,
        backend: MemoryCredentials,
    ) -> TwitchLogin<MemoryCredentials, FakeTwitch> {
        let accounts = Arc::new(TwitchAccounts::new(
            ConfigStore::new(directory.path()),
            TwitchCredentialStore::new(backend, CLIENT_ID),
        ));
        TwitchLogin::new(accounts, DeviceAuth::new(FakeTwitch))
    }

    fn coordinator() -> Arc<DesktopAuthentication> {
        Arc::new(DesktopAuthentication::new(|_| Ok(())))
    }

    fn identity(attempt_id: &str) -> AuthenticationIdentity {
        AuthenticationIdentity {
            attempt_id: attempt_id.into(),
        }
    }

    fn code(coordinator: &DesktopAuthentication, public_id: &str, core_id: u64, url: &str) {
        coordinator.report(public_id, TwitchRole::Bot, core_id, LoginEvent::Starting);
        coordinator.report(
            public_id,
            TwitchRole::Bot,
            core_id,
            LoginEvent::Code {
                url: url.into(),
                code: "ABCDEFGH".into(),
            },
        );
    }

    #[test]
    fn replaced_and_cancelled_attempts_ignore_delayed_callbacks() {
        let coordinator = coordinator();
        code(&coordinator, "old", 1, ACTIVATION_URL);
        code(&coordinator, "current", 2, ACTIVATION_URL);
        coordinator.report(
            "old",
            TwitchRole::Bot,
            1,
            LoginEvent::Code {
                url: ACTIVATION_URL.into(),
                code: "OLD".into(),
            },
        );
        coordinator.report(
            "old",
            TwitchRole::Bot,
            1,
            LoginEvent::Review {
                login: "other".into(),
                user_id: "456".into(),
            },
        );
        coordinator.report(
            "old",
            TwitchRole::Bot,
            1,
            LoginEvent::Failed("private failure".into()),
        );
        coordinator.report(
            "old",
            TwitchRole::Bot,
            2,
            LoginEvent::Review {
                login: "other".into(),
                user_id: "456".into(),
            },
        );
        assert!(matches!(
            coordinator.snapshot().unwrap().unwrap().phase,
            TwitchAuthenticationPhase::Code { .. }
        ));
        assert_eq!(
            coordinator
                .verification_url(identity("old"))
                .unwrap_err()
                .code,
            AuthenticationErrorCode::StaleAttempt
        );
        {
            let mut state = coordinator.state.lock().unwrap();
            state.active.as_mut().unwrap().snapshot.phase = TwitchAuthenticationPhase::Cancelled;
        }
        coordinator.report(
            "current",
            TwitchRole::Bot,
            2,
            LoginEvent::Review {
                login: "other".into(),
                user_id: "456".into(),
            },
        );
        assert_eq!(
            coordinator.snapshot().unwrap().unwrap().phase,
            TwitchAuthenticationPhase::Cancelled
        );
    }

    #[test]
    fn verification_url_requires_current_code_and_exact_trusted_url() {
        let coordinator = coordinator();
        code(&coordinator, "current", 2, ACTIVATION_URL);
        assert_eq!(
            coordinator.verification_url(identity("current")).unwrap(),
            ACTIVATION_URL
        );
        coordinator.report(
            "current",
            TwitchRole::Bot,
            2,
            LoginEvent::Review {
                login: "streamerbot".into(),
                user_id: "456".into(),
            },
        );
        assert_eq!(
            coordinator
                .verification_url(identity("current"))
                .unwrap_err()
                .code,
            AuthenticationErrorCode::NotReady
        );
        for url in [
            "https://www.twitch.tv/activate?redirect=other",
            "https://www.twitch.tv/activate/",
            "https://www.twitch.tv.evil/activate",
        ] {
            code(&coordinator, "current", 3, url);
            assert_eq!(
                coordinator
                    .verification_url(identity("current"))
                    .unwrap_err()
                    .code,
                AuthenticationErrorCode::NotReady
            );
            let json = serde_json::to_string(&coordinator.snapshot().unwrap()).unwrap();
            assert!(!json.contains(url));
        }
    }

    #[test]
    fn wrong_identity_preserves_review_and_saving_rejects_replacement_or_cancel() {
        let directory = tempfile::tempdir().unwrap();
        let login = login(&directory, MemoryCredentials::default());
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let coordinator = coordinator();
        code(&coordinator, "current", 2, ACTIVATION_URL);
        coordinator.report(
            "current",
            TwitchRole::Bot,
            2,
            LoginEvent::Review {
                login: "streamerbot".into(),
                user_id: "456".into(),
            },
        );
        let before = coordinator.snapshot().unwrap();
        for (attempt_id, user_id) in [("old", "456"), ("current", "wrong")] {
            let error = coordinator
                .approve(
                    &login,
                    &spawner,
                    ApproveTwitchLogin {
                        attempt_id: attempt_id.into(),
                        user_id: user_id.into(),
                    },
                )
                .unwrap_err();
            assert_eq!(error.code, AuthenticationErrorCode::StaleAttempt);
            assert_eq!(coordinator.snapshot().unwrap(), before);
        }
        coordinator
            .state
            .lock()
            .unwrap()
            .active
            .as_mut()
            .unwrap()
            .snapshot
            .phase = TwitchAuthenticationPhase::Saving;
        assert_eq!(
            coordinator
                .start(&login, &spawner, TwitchRole::Broadcaster)
                .unwrap_err()
                .code,
            AuthenticationErrorCode::Busy
        );
        assert_eq!(
            coordinator
                .cancel(&login, identity("current"))
                .unwrap_err()
                .code,
            AuthenticationErrorCode::Busy
        );
        assert_eq!(
            coordinator.snapshot().unwrap().unwrap().phase,
            TwitchAuthenticationPhase::Saving
        );
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn rejected_scheduling_publishes_failed_without_writing_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let backend = MemoryCredentials::default();
        let login = login(&directory, backend.clone());
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let (tx, rx) = mpsc::channel();
        let coordinator = Arc::new(DesktopAuthentication::new(move |update| {
            tx.send(update).map_err(|error| error.to_string())
        }));
        let started = coordinator
            .start(&login, &spawner, TwitchRole::Bot)
            .unwrap();
        assert!(Uuid::parse_str(&started.attempt_id).is_ok());
        loop {
            let update = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let json = serde_json::to_string(&update).unwrap();
            assert!(!json.contains("private-"));
            if matches!(update.phase, TwitchAuthenticationPhase::Review { .. }) {
                break;
            }
        }
        runtime.request_shutdown();
        assert_eq!(
            coordinator
                .approve(
                    &login,
                    &spawner,
                    ApproveTwitchLogin {
                        attempt_id: started.attempt_id,
                        user_id: "456".into()
                    }
                )
                .unwrap_err()
                .code,
            AuthenticationErrorCode::Unavailable
        );
        assert!(matches!(
            coordinator.snapshot().unwrap().unwrap().phase,
            TwitchAuthenticationPhase::Failed { .. }
        ));
        assert!(backend.0.lock().unwrap().is_empty());
        assert_eq!(
            coordinator
                .start(&login, &spawner, TwitchRole::Bot)
                .unwrap_err()
                .code,
            AuthenticationErrorCode::Unavailable
        );
        assert!(matches!(
            coordinator.snapshot().unwrap().unwrap().phase,
            TwitchAuthenticationPhase::Failed { .. }
        ));
        assert!(backend.0.lock().unwrap().is_empty());
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn publication_failure_keeps_snapshot_and_returns_safe_command_error() {
        let directory = tempfile::tempdir().unwrap();
        let login = login(&directory, MemoryCredentials::default());
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let coordinator = Arc::new(DesktopAuthentication::new(|_| {
            Err("private sink error".into())
        }));
        let error = coordinator
            .start(&login, &spawner, TwitchRole::Bot)
            .unwrap_err();
        assert_eq!(error.code, AuthenticationErrorCode::Unavailable);
        assert!(!serde_json::to_string(&error).unwrap().contains("private"));
        let snapshot = coordinator.snapshot().unwrap().unwrap();
        assert_eq!(
            coordinator
                .cancel(&login, identity(&snapshot.attempt_id))
                .unwrap_err()
                .code,
            AuthenticationErrorCode::Unavailable
        );
        assert_eq!(
            coordinator.snapshot().unwrap().unwrap().phase,
            TwitchAuthenticationPhase::Cancelled
        );
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn reviewed_identity_persists_only_after_approval_and_publishes_a_recoverable_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let backend = MemoryCredentials::default();
        let login = login(&directory, backend.clone());
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let hub = Arc::new(crate::hub::DesktopHub::new());
        let mut updates = hub.subscribe();
        let published = Arc::clone(&hub);
        let (tx, rx) = mpsc::channel();
        let coordinator = Arc::new(DesktopAuthentication::new(move |attempt| {
            published.publish(crate::hub::DesktopEvent::TwitchAuthentication(
                attempt.clone(),
            ))?;
            tx.send(attempt).map_err(|error| error.to_string())
        }));
        let started = coordinator
            .start(&login, &spawner, TwitchRole::Bot)
            .unwrap();
        loop {
            let attempt = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            if matches!(attempt.phase, TwitchAuthenticationPhase::Review { .. }) {
                break;
            }
        }
        assert!(backend.0.lock().unwrap().is_empty());
        assert_eq!(
            hub.snapshot().unwrap().twitch_authentication,
            coordinator.snapshot().unwrap()
        );
        coordinator
            .approve(
                &login,
                &spawner,
                ApproveTwitchLogin {
                    attempt_id: started.attempt_id,
                    user_id: "456".into(),
                },
            )
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap().phase,
            TwitchAuthenticationPhase::Saving
        );
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap().phase,
            TwitchAuthenticationPhase::Connected { .. }
        ));
        assert!(!backend.0.lock().unwrap().is_empty());
        let snapshot = hub.snapshot().unwrap();
        assert_eq!(
            snapshot.twitch_authentication,
            coordinator.snapshot().unwrap()
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("private-"));
        for sequence in 1..=snapshot.sequence {
            assert_eq!(updates.try_recv().unwrap().sequence, sequence);
        }
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn a_successful_progress_publication_recovers_from_an_earlier_sink_failure() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let coordinator = DesktopAuthentication::new(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Err("fixture failure".into())
            } else {
                Ok(())
            }
        });
        code(&coordinator, "current", 1, ACTIVATION_URL);
        assert!(
            !coordinator
                .state
                .lock()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .publication_failed
        );
        assert!(matches!(
            coordinator.snapshot().unwrap().unwrap().phase,
            TwitchAuthenticationPhase::Code { .. }
        ));
    }
}
