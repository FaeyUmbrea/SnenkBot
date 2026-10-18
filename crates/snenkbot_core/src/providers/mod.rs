//! Compiled lookup providers shared by workflows, Lua, and the local overlay API.

mod actions;
mod integration;
mod settings;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

pub use integration::{ProviderIntegration, ProviderIntegrationError};
pub use settings::{
    BttvSettings, EmotesSettings, FfzSettings, LoadedProviderSettings, PronounsSettings,
    ProviderId, ProviderSettings, ProviderSettingsError, ProviderSettingsStore,
    STEAMGRIDDB_KEY_CREDENTIAL, SteamGridDbSettings,
};

/// A compiled operation available through the provider API and workflow engine.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ProviderOperation {
    pub id: &'static str,
    pub title: &'static str,
    pub action: &'static str,
}

/// A compiled provider and the operations it exposes.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderDescriptor {
    /// Settings owner for a service; aggregate compatibility operations have no separate settings.
    #[serde(skip)]
    pub configuration: Option<ProviderId>,
    pub id: &'static str,
    pub title: &'static str,
    pub operations: Vec<ProviderOperation>,
}

/// Lists provider permissions using the same action declarations registered for workflows.
pub fn catalog() -> Vec<ProviderDescriptor> {
    actions::catalog()
}

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_CACHE_ENTRIES: usize = 128;
const MAX_OUTSTANDING_CALLS: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(12);

/// A provider failure suitable for returning without exposing upstream response bodies.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("unknown provider or operation")]
    UnknownOperation,
    #[error("invalid provider arguments: {0}")]
    InvalidArguments(String),
    #[error("provider is disabled")]
    Disabled,
    #[error("SteamGridDB API key is not configured")]
    MissingCredential,
    #[error("provider credential could not be read")]
    CredentialUnavailable,
    #[error("provider request failed")]
    RequestFailed,
    #[error("provider returned an invalid response")]
    InvalidResponse,
    #[error("too many provider calls are active")]
    Busy,
    #[error("provider call timed out")]
    TimedOut,
    #[error("provider configuration changed during the call")]
    ConfigurationChanged,
}

/// Retrieves the SteamGridDB API key from credential storage at call time.
pub trait ProviderSecrets: Send + Sync {
    /// Returns the current key without logging or exposing it through the provider result.
    fn steamgriddb_key(&self) -> Result<Option<String>, ProviderError>;
}

/// An HTTP response containing only the status and validated size bounded JSON.
#[derive(Clone, Debug)]
pub struct ProviderResponse {
    pub status: u16,
    pub body: Value,
}

/// Transport boundary used by the compiled providers and offline tests.
pub trait ProviderTransport: Send + Sync {
    /// GETs a fixed provider URL with an optional bearer key.
    fn get<'a>(
        &'a self,
        url: String,
        bearer: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse, ProviderError>> + Send + 'a>>;
}

/// HTTPS transport with no redirect following, a short timeout, and a response size limit.
pub struct ReqwestProviderTransport(reqwest::Client);

impl ReqwestProviderTransport {
    /// Builds the shared HTTP client without contacting a remote provider.
    pub fn new() -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ProviderError::RequestFailed)?;
        Ok(Self(client))
    }
}

impl ProviderTransport for ReqwestProviderTransport {
    /// Returns only bounded JSON and a status code, with upstream errors sanitized.
    fn get<'a>(
        &'a self,
        url: String,
        bearer: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let mut request = self.0.get(url);
            if let Some(key) = bearer {
                request = request.bearer_auth(key);
            }
            let mut response = request
                .send()
                .await
                .map_err(|_| ProviderError::RequestFailed)?;
            let status = response.status().as_u16();
            if status >= 300 && status != 404 {
                return Err(ProviderError::RequestFailed);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| ProviderError::RequestFailed)?
            {
                if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Err(ProviderError::InvalidResponse);
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = if status == 404 {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidResponse)?
            };
            Ok(ProviderResponse { status, body })
        })
    }
}

/// A successful result retained until its provider-specific expiry.
#[derive(Clone)]
struct CachedValue {
    value: Value,
    expires: Instant,
}

/// Runtime lookup services; settings and secrets are checked before cache use.
pub struct ProviderServices {
    transport: Arc<dyn ProviderTransport>,
    secrets: Arc<dyn ProviderSecrets>,
    settings: RwLock<ProviderSettings>,
    cache: Mutex<HashMap<String, CachedValue>>,
    requests: AsyncMutex<()>,
    capacity: Semaphore,
    generation: AtomicU64,
}

impl ProviderServices {
    /// Creates providers with the bounded HTTPS transport; no requests start here.
    pub fn new(
        settings: ProviderSettings,
        secrets: Arc<dyn ProviderSecrets>,
    ) -> Result<Self, ProviderError> {
        Ok(Self::with_transport(
            settings,
            secrets,
            Arc::new(ReqwestProviderTransport::new()?),
        ))
    }

    /// Creates providers with an injected transport for offline verification.
    pub fn with_transport(
        settings: ProviderSettings,
        secrets: Arc<dyn ProviderSecrets>,
        transport: Arc<dyn ProviderTransport>,
    ) -> Self {
        Self {
            transport,
            secrets,
            settings: RwLock::new(settings),
            cache: Mutex::new(HashMap::new()),
            requests: AsyncMutex::new(()),
            capacity: Semaphore::new(MAX_OUTSTANDING_CALLS),
            generation: AtomicU64::new(0),
        }
    }

    /// Replaces the enabled switches after a validated settings save.
    pub fn set_settings(&self, settings: ProviderSettings) {
        let mut current = self.settings.write().expect("provider settings lock");
        *current = settings;
        self.clear_cache();
    }

    /// Drops previous lookup results after a credential change.
    pub fn clear_cache(&self) {
        let mut cache = self.cache.lock().expect("provider cache lock");
        self.generation.fetch_add(1, Ordering::SeqCst);
        cache.clear();
    }

    /// Returns the current enabled switches for settings reads.
    pub fn settings(&self) -> ProviderSettings {
        self.settings
            .read()
            .expect("provider settings lock")
            .clone()
    }

    /// Reports whether a named provider can run now, including its required key.
    pub fn ready(&self, provider: &str) -> Result<(), ProviderError> {
        let settings = self.settings();
        let enabled = match provider {
            "steamgriddb" => settings.steamgriddb_enabled,
            "emotes" => ProviderId::Ffz.enabled(&settings) || ProviderId::Bttv.enabled(&settings),
            "ffz" => ProviderId::Ffz.enabled(&settings),
            "bttv" => ProviderId::Bttv.enabled(&settings),
            "pronouns" => settings.pronouns_enabled,
            _ => return Err(ProviderError::UnknownOperation),
        };
        if !enabled {
            return Err(ProviderError::Disabled);
        }
        if provider == "steamgriddb" && self.steamgriddb_key()?.is_none() {
            return Err(ProviderError::MissingCredential);
        }
        Ok(())
    }

    /// Calls a registered lookup operation with a typed, validated argument object.
    pub async fn call(
        &self,
        provider: &str,
        operation: &str,
        arguments: Value,
    ) -> Result<Value, ProviderError> {
        let generation = self.generation.load(Ordering::SeqCst);
        self.ready(provider)?;
        let _permit = self
            .capacity
            .try_acquire()
            .map_err(|_| ProviderError::Busy)?;
        let result = tokio::time::timeout(
            CALL_TIMEOUT,
            self.call_operation(provider, operation, arguments),
        )
        .await
        .map_err(|_| ProviderError::TimedOut)??;
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(ProviderError::ConfigurationChanged);
        }
        self.ready(provider)?;
        Ok(result)
    }

    /// Dispatches one already authorized operation to its typed provider logic.
    async fn call_operation(
        &self,
        provider: &str,
        operation: &str,
        arguments: Value,
    ) -> Result<Value, ProviderError> {
        match (provider, operation) {
            ("steamgriddb", "search") => {
                let args: NameArguments = parse_arguments(arguments)?;
                self.search(&valid_name(&args.name)?).await
            }
            ("steamgriddb", "icons") => {
                let args: GameIdArguments = parse_arguments(arguments)?;
                self.icons(args.game_id).await
            }
            ("steamgriddb", "resolve") => {
                let args: ResolveArguments = parse_arguments(arguments)?;
                self.resolve(&valid_name(&args.name)?, args.game_id).await
            }
            ("emotes" | "ffz" | "bttv", "resolve") => {
                let args: ChannelArguments = parse_arguments(arguments)?;
                self.emotes(&valid_channel_id(&args.channel_id)?, provider)
                    .await
            }
            ("pronouns", "resolve") => {
                let args: LoginArguments = parse_arguments(arguments)?;
                self.pronouns(&valid_login(&args.login)?).await
            }
            _ => Err(ProviderError::UnknownOperation),
        }
    }

    /// Reads the current SteamGridDB key without retaining it in this service.
    fn steamgriddb_key(&self) -> Result<Option<String>, ProviderError> {
        self.secrets
            .steamgriddb_key()
            .map(|key| key.filter(|value| !value.trim().is_empty()))
    }

    /// Shares concurrent lookups and stores a successful bounded result for a fixed TTL.
    async fn cached<F, Fut>(
        &self,
        key: String,
        ttl: Duration,
        load: F,
    ) -> Result<Value, ProviderError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, ProviderError>>,
    {
        if let Some(value) = self.cached_value(&key) {
            return Ok(value);
        }
        // The second cache check makes simultaneous lookups share one upstream request.
        let _guard = self.requests.lock().await;
        if let Some(value) = self.cached_value(&key) {
            return Ok(value);
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let value = load().await?;
        let mut cache = self.cache.lock().expect("provider cache lock");
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(ProviderError::ConfigurationChanged);
        }
        if cache.len() >= MAX_CACHE_ENTRIES
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, value)| value.expires)
                .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(
            key,
            CachedValue {
                value: value.clone(),
                expires: Instant::now() + ttl,
            },
        );
        Ok(value)
    }

    /// Returns a currently valid cached result for one provider lookup.
    fn cached_value(&self, key: &str) -> Option<Value> {
        self.cache
            .lock()
            .expect("provider cache lock")
            .get(key)
            .filter(|entry| entry.expires > Instant::now())
            .map(|entry| entry.value.clone())
    }

    /// Searches SteamGridDB for games matching a validated title.
    async fn search(&self, name: &str) -> Result<Value, ProviderError> {
        let key = self
            .steamgriddb_key()?
            .ok_or(ProviderError::MissingCredential)?;
        let url = path_url(
            "https://www.steamgriddb.com/api/v2/search/autocomplete/",
            name,
        )?;
        self.cached(
            format!("steamgriddb:search:{name}"),
            Duration::from_secs(900),
            || async {
                let response = self.transport.get(url, Some(key)).await?;
                let envelope: SteamGridEnvelope<GameMatch> = decode(response)?;
                Ok(json!(envelope.data))
            },
        )
        .await
    }

    /// Gets static PNG icon candidates for a SteamGridDB game ID.
    async fn icons(&self, game_id: u64) -> Result<Value, ProviderError> {
        if game_id == 0 {
            return Err(ProviderError::InvalidArguments(
                "gameId must be positive".into(),
            ));
        }
        let key = self
            .steamgriddb_key()?
            .ok_or(ProviderError::MissingCredential)?;
        let url = format!(
            "https://www.steamgriddb.com/api/v2/icons/game/{game_id}?mimes=image%2Fpng&types=static"
        );
        self.cached(
            format!("steamgriddb:icons:{game_id}"),
            Duration::from_secs(3600),
            || async {
                let response = self.transport.get(url, Some(key)).await?;
                let envelope: SteamGridEnvelope<GameIcon> = decode(response)?;
                let icons: Vec<GameIcon> = envelope
                    .data
                    .into_iter()
                    .filter(|icon| is_https(&icon.url))
                    .collect();
                Ok(json!(icons))
            },
        )
        .await
    }

    /// Finds an exact title or explicit ID and selects its highest-rated HTTPS icon.
    async fn resolve(&self, name: &str, game_id: Option<u64>) -> Result<Value, ProviderError> {
        let matched = if let Some(id) = game_id {
            if id == 0 {
                return Err(ProviderError::InvalidArguments(
                    "gameId must be positive".into(),
                ));
            }
            Some(GameMatch {
                id,
                name: name.into(),
            })
        } else {
            let candidates: Vec<GameMatch> = serde_json::from_value(self.search(name).await?)
                .map_err(|_| ProviderError::InvalidResponse)?;
            candidates
                .into_iter()
                .find(|candidate| normalize_name(&candidate.name) == normalize_name(name))
        };
        let Some(game) = matched else {
            return Ok(json!({"name": name, "id": null, "icon": null}));
        };
        let mut icons: Vec<GameIcon> = serde_json::from_value(self.icons(game.id).await?)
            .map_err(|_| ProviderError::InvalidResponse)?;
        icons.sort_by(|left, right| {
            right
                .score
                .unwrap_or(0.0)
                .total_cmp(&left.score.unwrap_or(0.0))
        });
        Ok(json!({"id": game.id, "name": game.name, "icon": icons.first().map(|icon| &icon.url)}))
    }

    /// Merges available global and channel FFZ/BTTV emotes by name.
    async fn emotes(&self, channel_id: &str, provider: &str) -> Result<Value, ProviderError> {
        self.cached(
            format!("{provider}:emotes:{channel_id}"),
            Duration::from_secs(900),
            || async {
                let urls = [
                    (
                        "https://api.frankerfacez.com/v1/set/global".to_owned(),
                        "ffz",
                    ),
                    (
                        "https://api.betterttv.net/3/cached/emotes/global".to_owned(),
                        "bttv-global",
                    ),
                    (
                        format!("https://api.frankerfacez.com/v1/room/id/{channel_id}"),
                        "ffz",
                    ),
                    (
                        format!("https://api.betterttv.net/3/cached/users/twitch/{channel_id}"),
                        "bttv-channel",
                    ),
                ];
                let mut entries = std::collections::BTreeMap::new();
                let mut succeeded = 0;
                let settings = self.settings();
                let responses =
                    join_all(
                        urls.into_iter()
                            .filter(|(_, kind)| {
                                if *kind == "ffz" {
                                    provider != "bttv" && ProviderId::Ffz.enabled(&settings)
                                } else {
                                    provider != "ffz" && ProviderId::Bttv.enabled(&settings)
                                }
                            })
                            .map(|(url, kind)| async move {
                                (kind, self.transport.get(url, None).await)
                            }),
                    )
                    .await;
                for (kind, response) in responses {
                    let response = match response {
                        Ok(response) => response,
                        Err(_) => continue,
                    };
                    if response.status == 404 {
                        succeeded += 1;
                        continue;
                    }
                    let parsed = match kind {
                        "ffz" => ffz_emotes(response.body),
                        "bttv-global" => bttv_emotes(response.body, false),
                        _ => bttv_emotes(response.body, true),
                    };
                    if let Ok(found) = parsed {
                        succeeded += 1;
                        for (name, url) in found {
                            entries.insert(name, url);
                        }
                    }
                }
                if succeeded == 0 {
                    return Err(ProviderError::RequestFailed);
                }
                Ok(json!(
                    entries
                        .into_iter()
                        .map(|(name, url)| json!({"name": name, "url": url}))
                        .collect::<Vec<_>>()
                ))
            },
        )
        .await
    }

    /// Resolves a public pronoun label or a confirmed absence for a Twitch login.
    async fn pronouns(&self, login: &str) -> Result<Value, ProviderError> {
        self.cached(
            format!("pronouns:{login}"),
            Duration::from_secs(900),
            || async {
                let response = self
                    .transport
                    .get(
                        format!("https://api.pronouns.alejo.io/v1/users/{login}"),
                        None,
                    )
                    .await?;
                if response.status == 404 {
                    return Ok(json!({"pronouns": null}));
                }
                let user: PronounUser = decode(response)?;
                let catalog = self.catalog().await?;
                let primary = catalog.get(&user.pronoun_id);
                let alternate = user.alt_pronoun_id.as_ref().and_then(|id| catalog.get(id));
                let label = match primary {
                    Some(primary) if user.alt_pronoun_id.is_none() || alternate.is_some() => {
                        Some(if primary.singular {
                            primary.subject.clone()
                        } else {
                            format!(
                                "{}/{}",
                                primary.subject,
                                alternate.map_or(primary.object.as_str(), |entry| entry
                                    .subject
                                    .as_str())
                            )
                        })
                    }
                    _ => None,
                };
                Ok(json!({"pronouns": label}))
            },
        )
        .await
    }

    /// Loads and caches Alejo's pronoun catalog for user ID resolution.
    async fn catalog(&self) -> Result<HashMap<String, PronounEntry>, ProviderError> {
        // Catalog requests share the same URL and bounded cache, without nesting `cached`.
        if let Some(value) = self.cached_value("pronouns:catalog") {
            return serde_json::from_value(value).map_err(|_| ProviderError::InvalidResponse);
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let response = self
            .transport
            .get("https://api.pronouns.alejo.io/v1/pronouns".into(), None)
            .await?;
        let catalog: HashMap<String, PronounEntry> = decode(response)?;
        let value = serde_json::to_value(&catalog).map_err(|_| ProviderError::InvalidResponse)?;
        let mut cache = self.cache.lock().expect("provider cache lock");
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(ProviderError::ConfigurationChanged);
        }
        if cache.len() >= MAX_CACHE_ENTRIES
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, value)| value.expires)
                .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(
            "pronouns:catalog".into(),
            CachedValue {
                value,
                expires: Instant::now() + Duration::from_secs(86400),
            },
        );
        Ok(catalog)
    }
}

/// Deserializes API arguments while rejecting unknown or missing fields.
fn parse_arguments<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, ProviderError> {
    serde_json::from_value(value)
        .map_err(|_| ProviderError::InvalidArguments("unexpected or missing field".into()))
}

/// Trims and bounds a game name before it becomes a URL path segment.
fn valid_name(name: &str) -> Result<String, ProviderError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 150 || name.chars().any(char::is_control) {
        return Err(ProviderError::InvalidArguments(
            "name must be 1 to 150 printable characters".into(),
        ));
    }
    Ok(name.into())
}

/// Accepts only a bounded numeric Twitch channel ID.
fn valid_channel_id(id: &str) -> Result<String, ProviderError> {
    if id.is_empty() || id.len() > 25 || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ProviderError::InvalidArguments(
            "channelId must be a Twitch user ID".into(),
        ));
    }
    Ok(id.into())
}

/// Normalizes and validates a Twitch login before building its request URL.
fn valid_login(login: &str) -> Result<String, ProviderError> {
    let login = login.to_ascii_lowercase();
    if login.is_empty()
        || login.len() > 25
        || !login
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ProviderError::InvalidArguments(
            "login must be a Twitch login".into(),
        ));
    }
    Ok(login)
}

/// Appends one percent-encoded path segment to a fixed provider endpoint.
fn path_url(base: &str, segment: &str) -> Result<String, ProviderError> {
    let mut url = reqwest::Url::parse(base).map_err(|_| ProviderError::RequestFailed)?;
    url.path_segments_mut()
        .map_err(|_| ProviderError::RequestFailed)?
        .pop_if_empty()
        .push(segment);
    Ok(url.to_string())
}

/// Checks that a returned asset URL uses HTTPS and has a host.
fn is_https(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some())
}

/// Removes case and punctuation differences for exact game matching.
fn normalize_name(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

/// Parses one successful provider response without exposing its raw body on error.
fn decode<T: for<'de> Deserialize<'de>>(response: ProviderResponse) -> Result<T, ProviderError> {
    if response.status != 200 {
        return Err(ProviderError::RequestFailed);
    }
    serde_json::from_value(response.body).map_err(|_| ProviderError::InvalidResponse)
}

/// Selects the best available FFZ image URL for each returned emote.
fn ffz_emotes(body: Value) -> Result<Vec<(String, String)>, ProviderError> {
    let response: FfzResponse =
        serde_json::from_value(body).map_err(|_| ProviderError::InvalidResponse)?;
    let mut found = Vec::new();
    for set in response.sets.into_values() {
        for emote in set.emoticons {
            let urls = emote
                .animated
                .as_ref()
                .filter(|urls| !urls.is_empty())
                .unwrap_or(&emote.urls);
            let url = urls
                .get("2")
                .or_else(|| urls.get("1"))
                .or_else(|| urls.values().next());
            if let Some(url) = url {
                let normalized = if url.starts_with("//") {
                    format!("https:{url}")
                } else {
                    url.clone()
                };
                if is_https(&normalized) {
                    found.push((emote.name, normalized));
                }
            }
        }
    }
    Ok(found)
}

/// Converts BTTV emote IDs to fixed HTTPS CDN URLs.
fn bttv_emotes(body: Value, channel: bool) -> Result<Vec<(String, String)>, ProviderError> {
    let emotes = if channel {
        let user: BttvUser =
            serde_json::from_value(body).map_err(|_| ProviderError::InvalidResponse)?;
        user.channel_emotes
            .into_iter()
            .chain(user.shared_emotes)
            .collect()
    } else {
        serde_json::from_value::<Vec<BttvEmote>>(body)
            .map_err(|_| ProviderError::InvalidResponse)?
    };
    Ok(emotes
        .into_iter()
        .filter(|emote| {
            !emote.id.is_empty() && emote.id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(|emote| {
            (
                emote.code,
                format!("https://cdn.betterttv.net/emote/{}/2x", emote.id),
            )
        })
        .collect())
}

/// Search request accepted by the public provider API.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArguments {
    name: String,
}
/// Icon request accepted by the public provider API.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GameIdArguments {
    game_id: u64,
}
/// Exact game resolution request accepted by the public provider API.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveArguments {
    name: String,
    game_id: Option<u64>,
}
/// Twitch channel request accepted by the public provider API.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelArguments {
    channel_id: String,
}
/// Twitch login request accepted by the public provider API.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginArguments {
    login: String,
}
/// SteamGridDB search match returned to callers.
#[derive(Clone, Deserialize, Serialize)]
struct GameMatch {
    id: u64,
    name: String,
}
/// SteamGridDB icon candidate returned to callers.
#[derive(Clone, Deserialize, Serialize)]
struct GameIcon {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
}
/// SteamGridDB's common data envelope.
#[derive(Deserialize)]
struct SteamGridEnvelope<T> {
    data: Vec<T>,
}
/// FFZ response with all applicable emote sets.
#[derive(Deserialize)]
struct FfzResponse {
    sets: HashMap<String, FfzSet>,
}
/// One FFZ global or channel emote set.
#[derive(Deserialize)]
struct FfzSet {
    emoticons: Vec<FfzEmote>,
}
/// FFZ emote name and available image sizes.
#[derive(Deserialize)]
struct FfzEmote {
    name: String,
    urls: HashMap<String, String>,
    animated: Option<HashMap<String, String>>,
}
/// BTTV channel emotes and shared emotes.
#[derive(Deserialize)]
struct BttvUser {
    #[serde(rename = "channelEmotes")]
    channel_emotes: Vec<BttvEmote>,
    #[serde(rename = "sharedEmotes")]
    shared_emotes: Vec<BttvEmote>,
}
/// BTTV emote identity used to build its CDN URL.
#[derive(Deserialize)]
struct BttvEmote {
    id: String,
    code: String,
}
/// Alejo user's primary and alternate pronoun IDs.
#[derive(Deserialize)]
struct PronounUser {
    pronoun_id: String,
    alt_pronoun_id: Option<String>,
}
/// Alejo catalog entry used to format a public pronoun label.
#[derive(Deserialize, Serialize)]
struct PronounEntry {
    subject: String,
    object: String,
    singular: bool,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::{
        ProviderError, ProviderResponse, ProviderSecrets, ProviderServices, ProviderSettings,
        ProviderTransport,
    };

    /// Test credential source with no real keyring access.
    struct Secrets(Option<String>);

    impl ProviderSecrets for Secrets {
        /// Supplies an in-memory key so tests cannot touch the system keyring.
        fn steamgriddb_key(&self) -> Result<Option<String>, ProviderError> {
            Ok(self.0.clone())
        }
    }

    /// Test transport that returns registered JSON without using the network.
    #[derive(Default)]
    struct FakeTransport {
        responses: Mutex<HashMap<String, ProviderResponse>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeTransport {
        /// Adds one stable response for a fixed endpoint.
        fn respond(&self, url: &str, status: u16, body: Value) {
            self.responses
                .lock()
                .unwrap()
                .insert(url.into(), ProviderResponse { status, body });
        }
    }

    impl ProviderTransport for FakeTransport {
        /// Records every requested URL and rejects endpoints without a fixture.
        fn get<'a>(
            &'a self,
            url: String,
            _bearer: Option<String>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ProviderResponse, ProviderError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls.lock().unwrap().push(url.clone());
                self.responses
                    .lock()
                    .unwrap()
                    .get(&url)
                    .cloned()
                    .ok_or(ProviderError::RequestFailed)
            })
        }
    }

    /// Pauses a successful response while a settings change is applied.
    #[derive(Default)]
    struct BlockingTransport {
        started: Notify,
        release: Notify,
    }

    impl ProviderTransport for BlockingTransport {
        /// Holds the request open until a test changes provider state or cancels it.
        fn get<'a>(
            &'a self,
            _url: String,
            _bearer: Option<String>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ProviderResponse, ProviderError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(ProviderResponse {
                    status: 200,
                    body: json!({"data":[{"id":7,"name":"Game"}]}),
                })
            })
        }
    }

    /// Enables all providers with fake dependencies for each lookup test.
    fn services(transport: Arc<FakeTransport>) -> ProviderServices {
        ProviderServices::with_transport(
            ProviderSettings {
                steamgriddb_enabled: true,
                emotes_enabled: true,
                pronouns_enabled: true,
                ..Default::default()
            },
            Arc::new(Secrets(Some("test-key".into()))),
            transport,
        )
    }

    /// Requires a normalized exact title and selects the highest-rated HTTPS icon.
    #[tokio::test]
    async fn steamgriddb_resolve_requires_exact_name_and_prefers_high_score() {
        let transport = Arc::new(FakeTransport::default());
        transport.respond(
            "https://www.steamgriddb.com/api/v2/search/autocomplete/Some%20Game",
            200,
            json!({"data":[{"id":1,"name":"Another Game"},{"id":2,"name":"Some Game™"}]}),
        );
        transport.respond(
            "https://www.steamgriddb.com/api/v2/icons/game/2?mimes=image%2Fpng&types=static",
            200,
            json!({"data":[{"url":"http://bad.example/icon","score":99},{"url":"https://good.example/low","score":1},{"url":"https://good.example/high","score":5}]}),
        );
        let services = services(Arc::clone(&transport));
        let resolved = services
            .call("steamgriddb", "resolve", json!({"name":"Some Game"}))
            .await
            .unwrap();
        assert_eq!(
            resolved,
            json!({"id":2,"name":"Some Game™","icon":"https://good.example/high"})
        );
        let resolved_again = services
            .call("steamgriddb", "resolve", json!({"name":"Some Game"}))
            .await
            .unwrap();
        assert_eq!(resolved_again, resolved);
        assert_eq!(transport.calls.lock().unwrap().len(), 2);
    }

    /// Uses available emote sources while treating a confirmed missing user as no pronouns.
    #[tokio::test]
    async fn emotes_merge_available_sources_and_pronouns_distinguish_absence() {
        let transport = Arc::new(FakeTransport::default());
        transport.respond("https://api.frankerfacez.com/v1/set/global", 200, json!({"sets":{"3":{"emoticons":[{"name":"Cat","urls":{"2":"//cdn.frankerfacez.com/cat"}}]}}}));
        transport.respond(
            "https://api.betterttv.net/3/cached/emotes/global",
            200,
            json!([{"id":"abc123","code":"Dog"}]),
        );
        transport.respond(
            "https://api.frankerfacez.com/v1/room/id/123",
            404,
            Value::Null,
        );
        transport.respond(
            "https://api.betterttv.net/3/cached/users/twitch/123",
            404,
            Value::Null,
        );
        transport.respond(
            "https://api.pronouns.alejo.io/v1/users/missing",
            404,
            Value::Null,
        );
        let services = services(transport);
        assert_eq!(
            services
                .call("emotes", "resolve", json!({"channelId":"123"}))
                .await
                .unwrap(),
            json!([
                {"name":"Cat","url":"https://cdn.frankerfacez.com/cat"},
                {"name":"Dog","url":"https://cdn.betterttv.net/emote/abc123/2x"}
            ])
        );
        assert_eq!(
            services
                .call("pronouns", "resolve", json!({"login":"Missing"}))
                .await
                .unwrap(),
            json!({"pronouns":null})
        );
    }

    /// Rejects disabled or unconfigured lookups before contacting a provider.
    #[tokio::test]
    async fn disabled_or_unconfigured_provider_never_contacts_transport() {
        let transport = Arc::new(FakeTransport::default());
        let services = ProviderServices::with_transport(
            ProviderSettings::default(),
            Arc::new(Secrets(None)),
            transport.clone(),
        );
        assert!(matches!(
            services
                .call("steamgriddb", "search", json!({"name":"Game"}))
                .await,
            Err(ProviderError::Disabled)
        ));
        services.set_settings(ProviderSettings {
            steamgriddb_enabled: true,
            ..ProviderSettings::default()
        });
        assert!(matches!(
            services
                .call("steamgriddb", "search", json!({"name":"Game"}))
                .await,
            Err(ProviderError::MissingCredential)
        ));
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    /// Keeps upstream failure distinct from absence and hides its response body.
    #[tokio::test]
    async fn upstream_failure_is_distinct_from_absence_and_does_not_expose_body() {
        let transport = Arc::new(FakeTransport::default());
        transport.respond(
            "https://api.pronouns.alejo.io/v1/users/example",
            500,
            json!({"message":"private-upstream-detail"}),
        );
        let services = services(transport);
        let error = services
            .call("pronouns", "resolve", json!({"login":"example"}))
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::RequestFailed));
        assert!(!error.to_string().contains("private-upstream-detail"));
    }

    /// Rejects malformed or extra arguments before any outbound request.
    #[tokio::test]
    async fn bad_arguments_never_contact_provider() {
        let transport = Arc::new(FakeTransport::default());
        let services = services(Arc::clone(&transport));
        for arguments in [
            json!({"channelId":"../../other"}),
            json!({"channelId":"123","extra":true}),
            json!({"channelId":42}),
        ] {
            assert!(matches!(
                services.call("emotes", "resolve", arguments).await,
                Err(ProviderError::InvalidArguments(_))
            ));
        }
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    /// Shares one outbound request across simultaneous equivalent lookups.
    #[tokio::test]
    async fn concurrent_same_lookup_shares_one_request() {
        let transport = Arc::new(FakeTransport::default());
        transport.respond(
            "https://www.steamgriddb.com/api/v2/search/autocomplete/Game",
            200,
            json!({"data":[{"id":7,"name":"Game"}]}),
        );
        let services = services(Arc::clone(&transport));
        let (first, second) = tokio::join!(
            services.call("steamgriddb", "search", json!({"name":"Game"})),
            services.call("steamgriddb", "search", json!({"name":"Game"})),
        );
        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    /// Discards an in-flight result after the provider is disabled.
    #[tokio::test]
    async fn disabling_provider_during_request_discards_result_and_cache() {
        let transport = Arc::new(BlockingTransport::default());
        let services = Arc::new(ProviderServices::with_transport(
            ProviderSettings {
                steamgriddb_enabled: true,
                ..ProviderSettings::default()
            },
            Arc::new(Secrets(Some("test-key".into()))),
            transport.clone(),
        ));
        let running = tokio::spawn({
            let services = services.clone();
            async move {
                services
                    .call("steamgriddb", "search", json!({"name":"Game"}))
                    .await
            }
        });
        transport.started.notified().await;
        services.set_settings(ProviderSettings::default());
        transport.release.notify_one();
        assert!(matches!(
            running.await.unwrap(),
            Err(ProviderError::ConfigurationChanged)
        ));
        assert!(services.cache.lock().unwrap().is_empty());
    }

    /// Prevents a response using an old key from repopulating the cache.
    #[tokio::test]
    async fn clearing_cache_during_request_discards_old_key_result() {
        let transport = Arc::new(BlockingTransport::default());
        let services = Arc::new(ProviderServices::with_transport(
            ProviderSettings {
                steamgriddb_enabled: true,
                ..ProviderSettings::default()
            },
            Arc::new(Secrets(Some("test-key".into()))),
            transport.clone(),
        ));
        let running = tokio::spawn({
            let services = services.clone();
            async move {
                services
                    .call("steamgriddb", "search", json!({"name":"Game"}))
                    .await
            }
        });
        transport.started.notified().await;
        services.clear_cache();
        transport.release.notify_one();
        assert!(matches!(
            running.await.unwrap(),
            Err(ProviderError::ConfigurationChanged)
        ));
        assert!(services.cache.lock().unwrap().is_empty());
    }

    /// Refuses excess calls immediately instead of queuing without a bound.
    #[tokio::test]
    async fn too_many_calls_fail_without_waiting_or_contacting_provider() {
        let transport = Arc::new(FakeTransport::default());
        let services = services(transport.clone());
        let permits: Vec<_> = (0..super::MAX_OUTSTANDING_CALLS)
            .map(|_| services.capacity.try_acquire().unwrap())
            .collect();
        assert!(matches!(
            services
                .call("emotes", "resolve", json!({"channelId":"123"}))
                .await,
            Err(ProviderError::Busy)
        ));
        assert!(transport.calls.lock().unwrap().is_empty());
        drop(permits);
    }

    /// Releases a call slot and leaves no cache entry after cancellation.
    #[tokio::test]
    async fn cancelled_call_releases_capacity_and_does_not_cache() {
        let transport = Arc::new(BlockingTransport::default());
        let services = Arc::new(ProviderServices::with_transport(
            ProviderSettings {
                steamgriddb_enabled: true,
                ..ProviderSettings::default()
            },
            Arc::new(Secrets(Some("test-key".into()))),
            transport.clone(),
        ));
        let running = tokio::spawn({
            let services = services.clone();
            async move {
                services
                    .call("steamgriddb", "search", json!({"name":"Game"}))
                    .await
            }
        });
        transport.started.notified().await;
        running.abort();
        assert!(running.await.is_err());
        assert_eq!(
            services.capacity.available_permits(),
            super::MAX_OUTSTANDING_CALLS
        );
        assert!(services.cache.lock().unwrap().is_empty());
    }

    /// Keeps permission metadata aligned with registered action declarations.
    #[test]
    fn catalog_matches_compiled_action_names() {
        let catalog = super::catalog();
        assert_eq!(catalog.len(), 5);
        assert_eq!(catalog[0].id, "steamgriddb");
        assert_eq!(
            catalog[0]
                .operations
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec!["search", "icons", "resolve"]
        );
        assert_eq!(catalog[1].operations[0].action, "ffz.resolve");
        assert_eq!(catalog[2].operations[0].action, "bttv.resolve");
        assert_eq!(catalog[3].operations[0].action, "pronouns.resolve");
        assert!(
            catalog[..4]
                .iter()
                .all(|provider| provider.configuration.is_some())
        );
        assert_eq!(catalog[4].id, "emotes");
        assert!(catalog[4].configuration.is_none());
    }
    /// Individual and compatibility lookups only contact enabled emote services.
    #[tokio::test]
    async fn emotes_respect_independent_source_switches() {
        let transport = Arc::new(FakeTransport::default());
        transport.respond(
            "https://api.frankerfacez.com/v1/set/global",
            404,
            Value::Null,
        );
        transport.respond(
            "https://api.frankerfacez.com/v1/room/id/123",
            404,
            Value::Null,
        );
        transport.respond(
            "https://api.betterttv.net/3/cached/emotes/global",
            404,
            Value::Null,
        );
        transport.respond(
            "https://api.betterttv.net/3/cached/users/twitch/123",
            404,
            Value::Null,
        );
        let services = services(transport.clone());
        for (enabled, disabled, settings) in [
            (
                "ffz",
                "bttv",
                ProviderSettings {
                    ffz_enabled: true,
                    ..Default::default()
                },
            ),
            (
                "bttv",
                "ffz",
                ProviderSettings {
                    bttv_enabled: true,
                    ..Default::default()
                },
            ),
        ] {
            services.set_settings(settings);
            transport.calls.lock().unwrap().clear();
            assert!(matches!(
                services
                    .call(disabled, "resolve", json!({"channelId":"123"}))
                    .await,
                Err(ProviderError::Disabled)
            ));
            assert_eq!(
                services
                    .call(enabled, "resolve", json!({"channelId":"123"}))
                    .await
                    .unwrap(),
                json!([])
            );
            assert_eq!(
                services
                    .call("emotes", "resolve", json!({"channelId":"123"}))
                    .await
                    .unwrap(),
                json!([])
            );
            let calls = transport.calls.lock().unwrap();
            assert_eq!(calls.len(), 4);
            assert!(calls.iter().all(|url| if enabled == "ffz" {
                url.contains("frankerfacez")
            } else {
                url.contains("betterttv")
            }));
        }
        services.set_settings(ProviderSettings::default());
        assert!(matches!(
            services.ready("emotes"),
            Err(ProviderError::Disabled)
        ));
    }
}
