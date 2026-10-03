//! Recent messages sent by this application, used to suppress EventSub echoes.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

const MAX_IDS: usize = 2_048;

#[derive(Default)]
pub struct ChatEchoes {
    ids: Mutex<RecentIds>,
}

impl ChatEchoes {
    pub fn record(&self, message_id: String) {
        if !message_id.is_empty() {
            self.ids
                .lock()
                .expect("chat echo lock poisoned")
                .insert(message_id);
        }
    }

    pub fn contains(&self, message_id: &str) -> bool {
        self.ids
            .lock()
            .expect("chat echo lock poisoned")
            .contains(message_id)
    }
}

#[derive(Default)]
struct RecentIds {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl RecentIds {
    fn insert(&mut self, id: String) {
        if !self.seen.insert(id.clone()) {
            return;
        }
        self.order.push_back(id);
        if self.order.len() > MAX_IDS
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.seen.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_sent_ids_without_unbounded_growth() {
        let echoes = ChatEchoes::default();
        echoes.record("first".into());
        assert!(echoes.contains("first"));
        echoes.record("first".into());
        for index in 0..MAX_IDS {
            echoes.record(format!("sent-{index}"));
        }
        assert!(!echoes.contains("first"));
        assert!(echoes.contains("sent-2047"));
        echoes.record(String::new());
        assert!(!echoes.contains(""));
    }
}
