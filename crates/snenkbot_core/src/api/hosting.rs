//! Read-only consumer files served behind the consumer's current credential.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Component, Path as FilePath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use futures_util::stream;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::watch;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::ApiService;

/// Serves the configured root's index without exposing a directory listing.
pub(super) async fn index(
    State(service): State<Arc<ApiService>>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    serve(&service, &token, "", request).await
}

/// Serves one decoded relative asset path after checking the current consumer grant.
pub(super) async fn asset(
    State(service): State<Arc<ApiService>>,
    Path((token, path)): Path<(String, String)>,
    request: Request,
) -> Response {
    serve(&service, &token, &path, request).await
}

/// Authenticates each request and delegates file protocol behavior to the file service.
async fn serve(service: &ApiService, token: &str, path: &str, mut request: Request) -> Response {
    let Ok(_permit) = service.storage_slots.try_acquire() else {
        return private_response(StatusCode::TOO_MANY_REQUESTS.into_response());
    };
    if !service
        .store
        .configuration()
        .is_ok_and(|settings| settings.enabled)
    {
        return private_response(StatusCode::NOT_FOUND.into_response());
    }
    let response = match service.store.authenticate(token) {
        Ok(Some(consumer)) => match consumer.static_folder {
            Some(folder) => match confined_file(&folder, path).await {
                Some(file) => {
                    let html = file.extension().is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("html")
                            || extension.eq_ignore_ascii_case("htm")
                    });
                    if html {
                        for header in [
                            header::RANGE,
                            header::IF_RANGE,
                            header::IF_MODIFIED_SINCE,
                            header::IF_UNMODIFIED_SINCE,
                            header::IF_NONE_MATCH,
                            header::IF_MATCH,
                        ] {
                            request.headers_mut().remove(header);
                        }
                    }
                    let revision = if html {
                        match tokio::fs::canonicalize(&folder).await {
                            Ok(root) => folder_revision(root).await,
                            Err(_) => {
                                return private_response(StatusCode::NOT_FOUND.into_response());
                            }
                        }
                    } else {
                        0
                    };
                    let head = request.method() == axum::http::Method::HEAD;
                    match ServeFile::new(file).oneshot(request).await {
                        Ok(response) if html && response.status() == StatusCode::OK => {
                            inject_reload(response.into_response(), token, revision, head).await
                        }
                        Ok(response) => response.into_response(),
                        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
                None => StatusCode::NOT_FOUND.into_response(),
            },
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    private_response(response)
}

/// Resolves assets and index files inside the canonical root, including symlink targets.
async fn confined_file(folder: &str, relative: &str) -> Option<PathBuf> {
    // Axum has decoded the route parameter. Reject remaining escapes rather than decoding twice.
    if relative.contains(['%', '\\', '\0'])
        || relative.split('/').any(|part| part == "." || part == "..")
        || FilePath::new(relative)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let root = tokio::fs::canonicalize(folder).await.ok()?;
    if !tokio::fs::metadata(&root).await.ok()?.is_dir() {
        return None;
    }
    let mut candidate = tokio::fs::canonicalize(root.join(relative)).await.ok()?;
    if !candidate.starts_with(&root) {
        return None;
    }
    if tokio::fs::metadata(&candidate).await.ok()?.is_dir() {
        candidate = tokio::fs::canonicalize(candidate.join("index.html"))
            .await
            .ok()?;
    }
    if !candidate.starts_with(&root) || !tokio::fs::metadata(&candidate).await.ok()?.is_file() {
        return None;
    }
    Some(candidate)
}

/// Prevents cached asset access and credential-bearing referrers after a grant changes.
pub(super) fn private_response(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// Adds a same-origin external reload client without placing credentials in page referrers.
async fn inject_reload(response: Response, token: &str, revision: u64, head: bool) -> Response {
    let (mut parts, body) = response.into_parts();
    for header in [
        header::CONTENT_LENGTH,
        header::ETAG,
        header::LAST_MODIFIED,
        header::ACCEPT_RANGES,
    ] {
        parts.headers.remove(header);
    }
    if head {
        return Response::from_parts(parts, axum::body::Body::empty());
    }
    let Ok(bytes) = axum::body::to_bytes(body, 16 * 1024 * 1024).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut html = bytes.to_vec();
    html.extend_from_slice(
        format!(
            "\n<script src=\"/files/{token}/.snenkbot/reload.js?revision={revision}\"></script>\n"
        )
        .as_bytes(),
    );
    Response::from_parts(parts, axum::body::Body::from(html))
}

/// Returns the automatic reload client only while the hosting grant remains active.
pub(super) async fn reload_script(
    State(service): State<Arc<ApiService>>,
    Path(token): Path<String>,
) -> Response {
    if hosting_root(&service, &token).await.is_none() {
        return private_response(StatusCode::NOT_FOUND.into_response());
    }
    private_response(
        (
            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
            RELOAD_SCRIPT,
        )
            .into_response(),
    )
}

const RELOAD_SCRIPT: &str = r#"(() => {
  const source = new URL('events', document.currentScript.src);
  const events = new EventSource(source);
  let revision = new URL(document.currentScript.src).searchParams.get('revision') ?? undefined;
  events.addEventListener('revision', event => {
    if (revision !== undefined && revision !== event.data) {
      events.close();
      location.reload();
      return;
    }
    revision = event.data;
  });
  events.addEventListener('revoked', () => events.close());
  addEventListener('pagehide', () => events.close(), {once: true});
})();"#;

/// Shares one native recursive watcher among active pages for the same consumer root.
struct ReloadWatch {
    _watcher: Mutex<RecommendedWatcher>,
    changes: watch::Sender<u64>,
}

impl Drop for ReloadWatch {
    /// Removes idle weak registry entries when the final page releases its native watcher.
    fn drop(&mut self) {
        if let Some(registry) = WATCHERS.get()
            && let Ok(mut registry) = registry.try_lock()
        {
            registry.retain(|_, watcher| watcher.strong_count() > 0);
        }
    }
}

/// Keys watcher ownership by consumer and canonical folder without retaining credentials.
type WatchRegistry = BTreeMap<(String, PathBuf), Weak<ReloadWatch>>;

static WATCHERS: OnceLock<Mutex<WatchRegistry>> = OnceLock::new();

/// Holds one event stream's current grant, watcher, and change subscriptions.
struct ReloadStream {
    _permit: tokio::sync::OwnedSemaphorePermit,
    service: Arc<ApiService>,
    token: String,
    root: PathBuf,
    watcher: Arc<ReloadWatch>,
    changes: watch::Receiver<u64>,
    consumers: watch::Receiver<u64>,
    settings: watch::Receiver<super::ApiSettings>,
    initial: bool,
    revision: u64,
}

/// Watches output changes and ends immediately when the consumer's hosting grant changes.
pub(super) async fn reload_events(
    State(service): State<Arc<ApiService>>,
    Path(token): Path<String>,
) -> Response {
    let Ok(permit) = Arc::clone(&service.connection_slots).try_acquire_owned() else {
        return private_response(StatusCode::TOO_MANY_REQUESTS.into_response());
    };
    let consumers = service.store.subscribe_consumer_changes();
    let settings = service.store.subscribe();
    let Some((id, root)) = hosting_root(&service, &token).await else {
        return private_response(StatusCode::NOT_FOUND.into_response());
    };
    let Some(watcher) = reload_watcher(id, root.clone()) else {
        return private_response(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    let changes = watcher.changes.subscribe();
    let revision = folder_revision(root.clone()).await;
    let state = ReloadStream {
        _permit: permit,
        consumers,
        settings,
        service,
        token,
        root,
        changes,
        watcher,
        initial: true,
        revision,
    };
    let events = stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        loop {
            if state.initial {
                if !same_hosting_root(&state).await {
                    return Some((
                        Ok::<_, Infallible>(Event::default().event("revoked").data("")),
                        None,
                    ));
                }
                state.initial = false;
                return Some((
                    Ok::<_, Infallible>(
                        Event::default()
                            .event("revision")
                            .data(state.revision.to_string()),
                    ),
                    Some(state),
                ));
            }
            tokio::select! {
                _ = state.service.hosting_shutdown.cancelled() => return None,
                result = state.settings.changed() => {
                    if result.is_err() || !state.settings.borrow().enabled {
                        return Some((Ok(Event::default().event("revoked").data("")), None));
                    }
                }
                result = state.consumers.changed() => {
                    if result.is_err() || !same_hosting_root(&state).await {
                        return Some((Ok(Event::default().event("revoked").data("")), None));
                    }
                }
                result = state.changes.changed() => {
                    if result.is_err() {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    state.changes.borrow_and_update();
                    if !same_hosting_root(&state).await {
                        return Some((Ok(Event::default().event("revoked").data("")), None));
                    }
                    let revision = folder_revision(state.root.clone()).await;
                    if revision != state.revision {
                        state.revision = revision;
                        return Some((Ok(Event::default().event("revision").data(revision.to_string())), Some(state)));
                    }
                }
            }
        }
    });
    private_response(
        Sse::new(events)
            .keep_alive(KeepAlive::default())
            .into_response(),
    )
}

/// Resolves the current consumer token and canonical hosting folder on each request.
async fn hosting_root(service: &ApiService, token: &str) -> Option<(String, PathBuf)> {
    if !service.store.configuration().ok()?.enabled {
        return None;
    }
    let consumer = service.store.authenticate(token).ok()??;
    let root = tokio::fs::canonicalize(consumer.static_folder?)
        .await
        .ok()?;
    tokio::fs::metadata(&root)
        .await
        .ok()?
        .is_dir()
        .then_some((consumer.id, root))
}

/// Checks that an open reload stream still belongs to the same active folder grant.
async fn same_hosting_root(state: &ReloadStream) -> bool {
    // Keeping the watcher in this stream drops native resources with its final subscriber.
    let _watcher = &state.watcher;
    hosting_root(&state.service, &state.token)
        .await
        .is_some_and(|(_, root)| root == state.root)
}

/// Creates a shared native watcher and prunes weak entries from disconnected pages.
fn reload_watcher(id: String, root: PathBuf) -> Option<Arc<ReloadWatch>> {
    let mut registry = WATCHERS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .ok()?;
    registry.retain(|_, watcher| watcher.strong_count() > 0);
    let key = (id, root.clone());
    if let Some(watcher) = registry.get(&key).and_then(Weak::upgrade) {
        return Some(watcher);
    }
    let (changes, _) = watch::channel(0_u64);
    let updates = changes.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|event| !matches!(event.kind, notify::EventKind::Access(_))) {
            updates.send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    })
    .ok()?;
    watcher.watch(&root, RecursiveMode::Recursive).ok()?;
    let watcher = Arc::new(ReloadWatch {
        _watcher: Mutex::new(watcher),
        changes,
    });
    registry.insert(key, Arc::downgrade(&watcher));
    Some(watcher)
}

/// Computes a stable folder revision so reconnecting pages detect writes made while disconnected.
async fn folder_revision(root: PathBuf) -> u64 {
    tokio::task::spawn_blocking(move || {
        let mut hash = DefaultHasher::new();
        let mut pending = vec![root];
        while let Some(path) = pending.pop() {
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            path.hash(&mut hash);
            metadata.len().hash(&mut hash);
            metadata.modified().ok().hash(&mut hash);
            if metadata.is_dir()
                && let Ok(entries) = std::fs::read_dir(path)
            {
                let mut children: Vec<_> = entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .collect();
                children.sort();
                pending.extend(children);
            }
        }
        hash.finish()
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transformed HTML omits stale lengths and validators for GET and HEAD responses.
    #[tokio::test]
    async fn reload_injection_updates_html_response_headers() {
        for head in [false, true] {
            let original = Response::builder()
                .header(header::CONTENT_LENGTH, "13")
                .header(header::ETAG, "original")
                .header(header::LAST_MODIFIED, "old")
                .header(header::ACCEPT_RANGES, "bytes")
                .body(axum::body::Body::from("<html></html>"))
                .unwrap();
            let response = private_response(inject_reload(original, "sb_test", 42, head).await);
            for header in [
                header::CONTENT_LENGTH,
                header::ETAG,
                header::LAST_MODIFIED,
                header::ACCEPT_RANGES,
            ] {
                assert!(!response.headers().contains_key(header));
            }
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            if head {
                assert!(body.is_empty());
            } else {
                assert!(
                    std::str::from_utf8(&body)
                        .unwrap()
                        .contains("reload.js?revision=42")
                );
            }
        }
        let error = private_response(StatusCode::NOT_FOUND.into_response());
        assert_eq!(error.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(error.headers()[header::REFERRER_POLICY], "no-referrer");
    }

    /// Revision scans do not recurse into symlinked directories beyond the configured root.
    #[cfg(unix)]
    #[tokio::test]
    async fn revision_ignores_files_beyond_symlinked_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("public");
        let external = temporary.path().join("external");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let file = external.join("secret");
        std::fs::write(&file, "before").unwrap();
        std::os::unix::fs::symlink(&external, root.join("linked")).unwrap();
        let before = folder_revision(root.clone()).await;
        std::fs::write(file, "changed outside hosting").unwrap();
        assert_eq!(before, folder_revision(root).await);
    }

    /// Native write notifications produce a new stable revision without retaining idle watchers.
    #[tokio::test]
    async fn native_output_writes_change_reload_revision() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let file = root.join("style.css");
        std::fs::write(&file, "body{}").unwrap();
        let watcher = reload_watcher("watch-test".into(), root.clone()).unwrap();
        let shared = reload_watcher("watch-test".into(), root.clone()).unwrap();
        assert!(Arc::ptr_eq(&watcher, &shared));
        let weak = Arc::downgrade(&watcher);
        let mut changes = watcher.changes.subscribe();
        let before = folder_revision(root.clone()).await;
        std::fs::write(file, "body{color:red}").unwrap();
        tokio::time::timeout(Duration::from_secs(5), changes.changed())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(before, folder_revision(root).await);
        drop(shared);
        drop(watcher);
        assert!(weak.upgrade().is_none());
    }

    /// Allows regular files and index pages while rejecting traversal and directory listings.
    #[tokio::test]
    async fn resolves_only_files_within_the_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("public");
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("index.html"), "hello").unwrap();
        std::fs::write(root.join("style.css"), "body{}").unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let folder = root.to_str().unwrap();
        assert_eq!(
            confined_file(folder, "").await,
            Some(canonical_root.join("index.html"))
        );
        assert_eq!(
            confined_file(folder, "style.css").await,
            Some(canonical_root.join("style.css"))
        );
        for path in [
            "empty",
            "../secret",
            "%2e%2e/secret",
            "a/../style.css",
            "/style.css",
            "a\\style.css",
        ] {
            assert!(confined_file(folder, path).await.is_none());
        }
        #[cfg(unix)]
        {
            let alias = temporary.path().join("linked-public");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            assert_eq!(
                confined_file(alias.to_str().unwrap(), "style.css").await,
                Some(canonical_root.join("style.css"))
            );
        }
    }

    /// Rejects both directly linked files and linked directory index files outside the root.
    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escapes_including_indexes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("public");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let secret = temporary.path().join("secret.html");
        std::fs::write(&secret, "private").unwrap();
        std::os::unix::fs::symlink(&secret, root.join("linked.html")).unwrap();
        std::os::unix::fs::symlink(&secret, root.join("nested/index.html")).unwrap();
        let folder = root.to_str().unwrap();
        assert!(confined_file(folder, "linked.html").await.is_none());
        assert!(confined_file(folder, "nested").await.is_none());
    }
}
