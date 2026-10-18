//! Bounded cache of original Twitch name colours, shared by chat and support events.

use std::collections::BTreeMap;
use std::time::Duration;
use tokio::time::Instant;

/// Accepts Twitch's complete hexadecimal RGB colour format.
pub(crate) fn valid_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

/// Keeps positive, unset and temporarily unavailable lookups without unbounded viewer retention.
#[derive(Default)]
pub(crate) struct NameColors {
    entries: BTreeMap<String, (Instant, Option<String>)>,
}

impl NameColors {
    /// Returns a cached result, including an explicitly unset colour, before its expiry.
    pub fn get(&self, id: &str, now: Instant) -> Option<Option<String>> {
        self.entries
            .get(id)
            .filter(|(expires, _)| *expires > now)
            .map(|(_, color)| color.clone())
    }

    /// Stores a result for its TTL, pruning expired entries and the oldest expiry at capacity.
    pub fn remember(&mut self, id: &str, color: Option<String>, ttl: Duration, now: Instant) {
        self.entries.retain(|_, (expires, _)| *expires > now);
        if self.entries.len() >= 2048
            && !self.entries.contains_key(id)
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (expires, _))| *expires)
                .map(|(id, _)| id.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(id.to_owned(), (now + ttl, color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset results avoid repeated requests and expire just like populated results.
    #[test]
    fn caches_unset_and_expires() {
        let mut cache = NameColors::default();
        let now = Instant::now();
        cache.remember("1", None, Duration::from_secs(30), now);
        assert_eq!(cache.get("1", now), Some(None));
        assert_eq!(cache.get("1", now + Duration::from_secs(30)), None);
        cache.remember("1", Some("#123456".into()), Duration::from_secs(60), now);
        assert_eq!(cache.get("1", now), Some(Some("#123456".into())));
    }

    /// Viewer churn cannot grow the cache beyond its fixed capacity.
    #[test]
    fn bounded_capacity() {
        let mut cache = NameColors::default();
        for id in 0..2100 {
            cache.remember(
                &id.to_string(),
                None,
                Duration::from_secs(60),
                Instant::now(),
            );
        }
        assert_eq!(cache.entries.len(), 2048);
        assert!(valid_color("#aB0123"));
        assert!(!valid_color("red"));
        assert!(!valid_color("#abcdef00"));
    }
}
