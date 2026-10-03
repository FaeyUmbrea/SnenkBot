//! Settled title/game changes, independent of other channel.update properties.

use super::events::ChannelUpdated;
use std::time::Duration;
use tokio::time::Instant;

const SETTLE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct ChannelChanges {
    delivered: Option<ChannelUpdated>,
    latest: Option<ChannelUpdated>,
    deadline: Option<Instant>,
}

fn same_details(a: &ChannelUpdated, b: &ChannelUpdated) -> bool {
    a.title == b.title && a.category_id == b.category_id && a.category_name == b.category_name
}

impl ChannelChanges {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe(&mut self, event: ChannelUpdated, now: Instant) {
        if self
            .delivered
            .as_ref()
            .is_none_or(|previous| previous.broadcaster_user_id != event.broadcaster_user_id)
        {
            self.delivered = Some(event.clone());
            self.latest = Some(event);
            self.deadline = None;
            return;
        }
        if self
            .latest
            .as_ref()
            .is_some_and(|latest| same_details(latest, &event))
        {
            return;
        }
        self.deadline = if self
            .delivered
            .as_ref()
            .is_some_and(|previous| same_details(previous, &event))
        {
            None
        } else {
            Some(now + SETTLE)
        };
        self.latest = Some(event);
    }

    pub fn initialized_for(&self, id: &str) -> bool {
        self.delivered
            .as_ref()
            .is_some_and(|event| event.broadcaster_user_id == id)
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn take_ready(&mut self, now: Instant) -> Option<ChannelUpdated> {
        if !self.deadline.is_some_and(|deadline| now >= deadline) {
            return None;
        }
        self.deadline = None;
        let event = self.latest.clone()?;
        self.delivered = Some(event.clone());
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn details(title: &str, game: &str) -> ChannelUpdated {
        ChannelUpdated {
            broadcaster_user_id: "owner".into(),
            broadcaster_user_login: "owner".into(),
            broadcaster_user_name: "Owner".into(),
            title: title.into(),
            category_id: game.into(),
            category_name: game.into(),
            language: "en".into(),
        }
    }
    #[test]
    fn groups_changes_ignores_duplicates_and_uses_latest_pair() {
        let now = Instant::now();
        let mut changes = ChannelChanges::default();
        changes.observe(details("Old", "Old game"), now);
        changes.observe(details("Old", "New game"), now);
        changes.observe(details("New", "New game"), now + Duration::from_secs(1));
        changes.observe(
            details("New", "New game"),
            now + Duration::from_millis(2500),
        );
        assert!(
            changes
                .take_ready(now + Duration::from_millis(2999))
                .is_none()
        );
        assert_eq!(
            changes.take_ready(now + Duration::from_secs(3)),
            Some(details("New", "New game"))
        );
        assert!(changes.take_ready(now + Duration::from_secs(4)).is_none());
    }
    #[test]
    fn ignores_language_only_changes_and_reverted_changes_and_resets_accounts() {
        let now = Instant::now();
        let mut changes = ChannelChanges::default();
        changes.observe(details("Old", "Game"), now);
        let mut language = details("Old", "Game");
        language.language = "de".into();
        changes.observe(language, now);
        assert!(changes.deadline().is_none());
        changes.observe(details("New", "Game"), now);
        changes.observe(details("Old", "Game"), now + Duration::from_secs(1));
        assert!(changes.deadline().is_none());
        changes.observe(details("New", "Game"), now);
        let mut other = details("Other", "Game");
        other.broadcaster_user_id = "other".into();
        changes.observe(other, now);
        assert!(changes.deadline().is_none());
        changes.reset();
        assert!(!changes.initialized_for("owner"));
    }
}
