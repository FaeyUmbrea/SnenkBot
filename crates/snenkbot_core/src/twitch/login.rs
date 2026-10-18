//! Device sign-in coordination. Tokens stay here until the displayed identity is approved.

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::runtime::RuntimeSpawner;

use super::accounts::{PendingAccount, TwitchAccounts, review_account, scopes_for};
use super::credentials::{CredentialBackend, SystemCredentialBackend, TwitchRole};
use super::device::{DeviceAuth, FormTransport, ReqwestTransport};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoginEvent {
    Starting,
    Code { url: String, code: String },
    Review { login: String, user_id: String },
    Connected { role: TwitchRole, login: String },
    Failed(String),
}

struct Attempt {
    id: u64,
    cancel: CancellationToken,
    pending: Option<PendingAccount>,
    committing: bool,
}

#[derive(Default)]
struct State {
    next_id: u64,
    active: Option<Attempt>,
}

pub struct TwitchLogin<B = SystemCredentialBackend, T = ReqwestTransport> {
    accounts: Arc<TwitchAccounts<B>>,
    auth: Arc<DeviceAuth<T>>,
    state: Arc<Mutex<State>>,
}

impl<B, T> TwitchLogin<B, T>
where
    B: CredentialBackend + 'static,
    T: FormTransport + Send + Sync + 'static,
{
    pub fn new(accounts: Arc<TwitchAccounts<B>>, auth: DeviceAuth<T>) -> Self {
        Self {
            accounts,
            auth: Arc::new(auth),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    pub fn start(
        &self,
        runtime: &RuntimeSpawner,
        role: TwitchRole,
        report: impl Fn(u64, LoginEvent) + Send + Sync + 'static,
    ) -> Result<u64, String> {
        let state = Arc::clone(&self.state);
        let auth = Arc::clone(&self.auth);
        let report = Arc::new(report);
        let cancel = runtime.cancellation_token();
        let id = {
            let mut state = state.lock().expect("Twitch login lock poisoned");
            if state.active.as_ref().is_some_and(|old| old.committing) {
                return Err("Twitch account approval is still being saved".into());
            }
            let id = state
                .next_id
                .checked_add(1)
                .ok_or("Twitch sign-in attempt limit reached")?;
            if let Some(old) = state.active.take() {
                old.cancel.cancel();
            }
            state.next_id = id;
            state.active = Some(Attempt {
                id,
                cancel: cancel.clone(),
                pending: None,
                committing: false,
            });
            id
        };
        report(id, LoginEvent::Starting);
        let scheduled = runtime
            .spawn_task("Twitch device sign-in", move |_| async move {
                let result = async {
                    let session = auth.begin(scopes_for(role), &cancel).await?;
                    if current(&state, id) {
                        report(
                            id,
                            LoginEvent::Code {
                                url: session.verification_uri.clone(),
                                code: session.user_code.clone(),
                            },
                        );
                    }
                    let tokens = auth.poll(&session, &cancel).await?;
                    let pending = review_account(&auth, role, tokens, &cancel).await?;
                    let event = LoginEvent::Review {
                        login: pending.login().to_owned(),
                        user_id: pending.user_id().to_owned(),
                    };
                    let mut state = state.lock().expect("Twitch login lock poisoned");
                    if let Some(active) = state.active.as_mut()
                        && active.id == id
                        && !active.cancel.is_cancelled()
                    {
                        active.pending = Some(pending);
                        drop(state);
                        report(id, event);
                    }
                    Ok::<(), super::accounts::AccountError>(())
                }
                .await;
                if let Err(error) = result
                    && current(&state, id)
                {
                    report(id, LoginEvent::Failed(error.to_string()));
                }
                Ok::<(), std::convert::Infallible>(())
            })
            .map_err(|error| error.to_string());
        if let Err(error) = scheduled {
            clear_attempt(&self.state, id);
            return Err(error);
        }
        Ok(id)
    }

    pub fn approve(
        &self,
        runtime: &RuntimeSpawner,
        user_id: &str,
        report: impl Fn(u64, LoginEvent) + Send + Sync + 'static,
    ) -> Result<(), String> {
        self.approve_current(runtime, None, user_id, report)
    }

    /// A delayed desktop command must not approve a replacement attempt, even for the same user.
    pub fn approve_attempt(
        &self,
        runtime: &RuntimeSpawner,
        attempt_id: u64,
        user_id: &str,
        report: impl Fn(u64, LoginEvent) + Send + Sync + 'static,
    ) -> Result<(), String> {
        self.approve_current(runtime, Some(attempt_id), user_id, report)
    }

    fn approve_current(
        &self,
        runtime: &RuntimeSpawner,
        expected_id: Option<u64>,
        user_id: &str,
        report: impl Fn(u64, LoginEvent) + Send + Sync + 'static,
    ) -> Result<(), String> {
        let (id, pending) = {
            let mut state = self.state.lock().expect("Twitch login lock poisoned");
            let attempt = state.active.as_mut().ok_or("No Twitch sign-in is active")?;
            if expected_id.is_some_and(|expected| expected != attempt.id) {
                return Err("The Twitch sign-in attempt changed".into());
            }
            let pending = attempt
                .pending
                .take()
                .ok_or("Twitch sign-in is not ready")?;
            if pending.user_id() != user_id {
                attempt.pending = Some(pending);
                return Err("The reviewed Twitch account changed".into());
            }
            attempt.committing = true;
            (attempt.id, pending)
        };
        let login = pending.login().to_owned();
        let role = pending.role();
        let accounts = Arc::clone(&self.accounts);
        let state = Arc::clone(&self.state);
        let report = Arc::new(report);
        let approved = user_id.to_owned();
        let scheduled = runtime
            .spawn_task("Twitch account approval", move |_| async move {
                let result =
                    tokio::task::spawn_blocking(move || accounts.accept(pending, &approved)).await;
                if current(&state, id) {
                    state.lock().expect("Twitch login lock poisoned").active = None;
                    match result {
                        Ok(Ok(_)) => {
                            report(id, LoginEvent::Connected { role, login });
                        }
                        Ok(Err(error)) => report(id, LoginEvent::Failed(error.to_string())),
                        Err(_) => report(
                            id,
                            LoginEvent::Failed("Twitch account worker stopped".into()),
                        ),
                    }
                }
                Ok::<(), std::convert::Infallible>(())
            })
            .map_err(|error| error.to_string());
        if scheduled.is_err() {
            clear_attempt(&self.state, id);
        }
        scheduled
    }

    pub fn cancel(&self) -> bool {
        self.cancel_current(None)
    }

    /// Cancels only the displayed attempt. Saving an approved identity cannot be interrupted.
    pub fn cancel_attempt(&self, attempt_id: u64) -> bool {
        self.cancel_current(Some(attempt_id))
    }

    fn cancel_current(&self, expected_id: Option<u64>) -> bool {
        let mut state = self.state.lock().expect("Twitch login lock poisoned");
        if expected_id.is_some_and(|expected| {
            state
                .active
                .as_ref()
                .is_none_or(|attempt| attempt.id != expected)
        }) || state
            .active
            .as_ref()
            .is_some_and(|attempt| attempt.committing)
        {
            return false;
        }
        if let Some(attempt) = state.active.take() {
            attempt.cancel.cancel();
        }
        true
    }
}

fn clear_attempt(state: &Mutex<State>, id: u64) {
    let mut state = state.lock().expect("Twitch login lock poisoned");
    if state
        .active
        .as_ref()
        .is_some_and(|attempt| attempt.id == id)
        && let Some(attempt) = state.active.take()
    {
        attempt.cancel.cancel();
    }
}

fn current(state: &Mutex<State>, id: u64) -> bool {
    state
        .lock()
        .expect("Twitch login lock poisoned")
        .active
        .as_ref()
        .is_some_and(|attempt| attempt.id == id && !attempt.cancel.is_cancelled())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::runtime::AppRuntime;
    use crate::storage::ConfigStore;
    use crate::twitch::credentials::{CredentialError, TwitchCredentialStore};
    use crate::twitch::device::{DeviceError, FormResponse};

    #[derive(Default)]
    struct MemoryCredentials(Mutex<HashMap<String, String>>);

    impl CredentialBackend for Arc<MemoryCredentials> {
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
                r#"{"device_code":"secret-code","user_code":"ABCDEFGH","verification_uri":"https://www.twitch.tv/activate","interval":1,"expires_in":30}"#
            } else {
                r#"{"access_token":"access-secret","refresh_token":"refresh-secret","expires_in":3600,"scope":["user:write:chat"],"token_type":"bearer"}"#
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
                body: format!(r#"{{"client_id":"{}","user_id":"456","login":"streamerbot","scopes":["user:write:chat"],"expires_in":3600}}"#, super::super::device::CLIENT_ID).into_bytes(),
                retry_after: None,
            })
        }
    }

    #[test]
    fn sign_in_requires_identity_approval_before_saving_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemoryCredentials::default());
        let accounts = Arc::new(TwitchAccounts::new(
            ConfigStore::new(dir.path()),
            TwitchCredentialStore::new(Arc::clone(&backend), super::super::device::CLIENT_ID),
        ));
        let login = TwitchLogin::new(Arc::clone(&accounts), DeviceAuth::new(FakeTwitch));
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let (tx, rx) = mpsc::channel();
        login
            .start(&spawner, TwitchRole::Bot, {
                let tx = tx.clone();
                move |_, event| tx.send(event).unwrap()
            })
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            LoginEvent::Starting
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            LoginEvent::Code {
                url: "https://www.twitch.tv/activate".into(),
                code: "ABCDEFGH".into(),
            }
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            LoginEvent::Review {
                login: "streamerbot".into(),
                user_id: "456".into(),
            }
        );
        assert!(accounts.connected().unwrap().bot.is_none());
        assert!(backend.0.lock().unwrap().is_empty());
        assert!(login.approve(&spawner, "wrong", |_, _| {}).is_err());
        login
            .approve(&spawner, "456", move |_, event| tx.send(event).unwrap())
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            LoginEvent::Connected {
                role: TwitchRole::Bot,
                login: "streamerbot".into(),
            }
        );
        assert_eq!(
            accounts.connected().unwrap().bot.unwrap().login,
            "streamerbot"
        );
        let json = std::fs::read_to_string(dir.path().join("twitch-accounts.json")).unwrap();
        assert!(!json.contains("access-secret"));
        assert!(!json.contains("refresh-secret"));
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn cancelling_a_code_stops_polling_without_saving_an_account() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemoryCredentials::default());
        let accounts = Arc::new(TwitchAccounts::new(
            ConfigStore::new(dir.path()),
            TwitchCredentialStore::new(Arc::clone(&backend), super::super::device::CLIENT_ID),
        ));
        let login = TwitchLogin::new(Arc::clone(&accounts), DeviceAuth::new(FakeTwitch));
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let (tx, rx) = mpsc::channel();
        login
            .start(&spawner, TwitchRole::Bot, move |_, event| {
                tx.send(event).unwrap()
            })
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            LoginEvent::Starting
        );
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            LoginEvent::Code { .. }
        ));
        assert!(login.cancel());
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert!(accounts.connected().unwrap().bot.is_none());
        assert!(backend.0.lock().unwrap().is_empty());
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    struct LoginFixture {
        _directory: tempfile::TempDir,
        backend: Arc<MemoryCredentials>,
        accounts: Arc<TwitchAccounts<Arc<MemoryCredentials>>>,
        login: TwitchLogin<Arc<MemoryCredentials>, FakeTwitch>,
    }

    impl LoginFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let backend = Arc::new(MemoryCredentials::default());
            let accounts = Arc::new(TwitchAccounts::new(
                ConfigStore::new(directory.path()),
                TwitchCredentialStore::new(Arc::clone(&backend), super::super::device::CLIENT_ID),
            ));
            let login = TwitchLogin::new(Arc::clone(&accounts), DeviceAuth::new(FakeTwitch));
            Self {
                _directory: directory,
                backend,
                accounts,
                login,
            }
        }

        fn review(&self, runtime: &RuntimeSpawner, role: TwitchRole) -> u64 {
            let (tx, rx) = mpsc::channel();
            let id = self
                .login
                .start(runtime, role, move |_, event| {
                    tx.send(event).unwrap();
                })
                .unwrap();
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                LoginEvent::Starting
            );
            assert!(matches!(
                rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                LoginEvent::Code { .. }
            ));
            assert!(matches!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                LoginEvent::Review { .. }
            ));
            id
        }
    }

    #[test]
    fn stale_commands_cannot_cancel_or_approve_a_replacement_for_the_same_identity() {
        let fixture = LoginFixture::new();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let old = fixture.review(&spawner, TwitchRole::Bot);
        let replacement = fixture.review(&spawner, TwitchRole::Bot);
        assert_ne!(old, replacement);
        assert!(!fixture.login.cancel_attempt(old));
        assert!(
            fixture
                .login
                .approve_attempt(&spawner, old, "456", |_, _| {})
                .is_err()
        );
        assert!(current(&fixture.login.state, replacement));
        assert!(fixture.backend.0.lock().unwrap().is_empty());
        let (tx, rx) = mpsc::channel();
        fixture
            .login
            .approve_attempt(&spawner, replacement, "456", move |_, event| {
                tx.send(event).unwrap();
            })
            .unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            LoginEvent::Connected {
                role: TwitchRole::Bot,
                ..
            }
        ));
        let connected = fixture.accounts.connected().unwrap();
        assert!(connected.broadcaster.is_none());
        assert!(connected.bot.is_some());
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn attempt_exhaustion_preserves_the_existing_sign_in() {
        let fixture = LoginFixture::new();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let id = fixture
            .login
            .start(&spawner, TwitchRole::Bot, |_, _| {})
            .unwrap();
        fixture.login.state.lock().unwrap().next_id = u64::MAX;
        assert!(
            fixture
                .login
                .start(&spawner, TwitchRole::Broadcaster, |_, _| {})
                .is_err()
        );
        assert!(current(&fixture.login.state, id));
        assert!(fixture.login.cancel_attempt(id));
        assert!(!fixture.login.cancel_attempt(id));
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn rejected_start_does_not_leave_an_active_attempt() {
        let fixture = LoginFixture::new();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        runtime.request_shutdown();
        assert!(
            fixture
                .login
                .start(&spawner, TwitchRole::Bot, |_, _| {})
                .is_err()
        );
        assert!(fixture.login.state.lock().unwrap().active.is_none());
        assert!(fixture.backend.0.lock().unwrap().is_empty());
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn rejected_approval_does_not_leave_an_unscheduled_save_or_write_credentials() {
        let fixture = LoginFixture::new();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let id = fixture.review(&spawner, TwitchRole::Bot);
        runtime.request_shutdown();
        assert!(
            fixture
                .login
                .approve_attempt(&spawner, id, "456", |_, _| {})
                .is_err()
        );
        assert!(fixture.login.state.lock().unwrap().active.is_none());
        assert!(fixture.backend.0.lock().unwrap().is_empty());
        assert!(fixture.accounts.connected().unwrap().bot.is_none());
        runtime.shutdown(Duration::from_secs(2)).unwrap();
    }
}
