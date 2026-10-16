//! Hand-off intents that arrive before the webview is listening (M-69).
//!
//! The Browser Companion and a second launch can deliver a URL while the
//! webview is still loading. An event emitted then reaches nobody, although the
//! sender was told the hand-off worked. The inbox holds those intents until the
//! frontend calls `take_pending_intents` on mount. That call also marks the
//! webview as listening, so later intents are emitted live. Both steps happen
//! under one lock, so an intent is never delivered twice and never lost.

use std::sync::Mutex;

pub struct IntentInbox<T> {
    state: Mutex<InboxState<T>>,
}

struct InboxState<T> {
    listening: bool,
    pending: Vec<T>,
}

impl<T> Default for IntentInbox<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(InboxState {
                listening: false,
                pending: Vec::new(),
            }),
        }
    }
}

impl<T> IntentInbox<T> {
    /// Returns the intent when the webview is listening and it should be emitted
    /// now. Otherwise stores it and returns `None`.
    pub fn deliver(&self, intent: T) -> Option<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.listening {
            Some(intent)
        } else {
            state.pending.push(intent);
            None
        }
    }

    /// Take every stored intent and start emitting live from now on.
    pub fn take_pending(&self) -> Vec<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.listening = true;
        std::mem::take(&mut state.pending)
    }

    /// The webview went away (reload or crash): store intents again until it asks.
    pub fn stop_listening(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.listening = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_before_mount_is_kept_until_the_frontend_pulls_it() {
        let inbox = IntentInbox::default();
        assert_eq!(inbox.deliver("early"), None);
        assert_eq!(inbox.take_pending(), vec!["early"]);
        assert!(inbox.take_pending().is_empty());
    }

    #[test]
    fn intent_after_mount_is_emitted_live_and_not_stored() {
        let inbox = IntentInbox::default();
        assert!(inbox.take_pending().is_empty());
        assert_eq!(inbox.deliver("live"), Some("live"));
        assert!(inbox.take_pending().is_empty());
    }

    #[test]
    fn intents_keep_arrival_order() {
        let inbox = IntentInbox::default();
        inbox.deliver(1);
        inbox.deliver(2);
        inbox.deliver(3);
        assert_eq!(inbox.take_pending(), vec![1, 2, 3]);
    }

    #[test]
    fn stop_listening_buffers_again() {
        let inbox = IntentInbox::default();
        inbox.take_pending();
        inbox.stop_listening();
        assert_eq!(inbox.deliver("reload"), None);
        assert_eq!(inbox.take_pending(), vec!["reload"]);
    }
}
