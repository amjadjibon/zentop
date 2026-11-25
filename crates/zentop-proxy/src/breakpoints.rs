//! The queue of messages held at breakpoints, shared by the engine and the GUI.
//!
//! A held message waits in its own connection's task on a oneshot channel, so
//! holding one never blocks another connection. The queue holds only what the
//! editor shows; the engine keeps the message itself and applies the edits.
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::oneshot;

use crate::presentation::{self, Secrets};
use crate::{Headers, ReplayHeader};

/// A held message as the Breakpoints window shows it.
#[derive(Clone, Debug)]
pub struct HeldMessage {
    pub id: u64,
    /// The flow it belongs to, unless capture was paused when it started.
    pub flow: Option<u64>,
    pub rule: String,
    /// A response rather than a request.
    pub response: bool,
    pub method: String,
    /// Masked unless revealed.
    pub url: String,
    pub status: Option<u16>,
    /// Masked unless revealed. Edits refer to these rows by index.
    pub headers: Headers,
    /// The body as text, when it is editable UTF-8.
    pub body: Option<String>,
    /// Whether the body was read and can be replaced.
    pub body_editable: bool,
    /// Why the body is not shown or editable.
    pub body_note: Option<&'static str>,
    pub held_for: Duration,
    pub remaining: Duration,
}

/// Edits applied when a held message is executed. Fields left as `None`
/// keep their held values; untouched masked values keep their originals.
#[derive(Debug, Default)]
pub struct HeldEdits {
    pub method: Option<String>,
    pub url: Option<String>,
    pub preserve_masked_query: bool,
    pub status: Option<u16>,
    pub headers: Option<Vec<ReplayHeader>>,
    pub body: Option<String>,
}

pub(crate) enum Outcome {
    Execute(HeldEdits),
    Abort,
    Release(&'static str),
}

/// What the engine shares with the editor when it holds a message.
pub(crate) struct Pending {
    pub flow: Option<u64>,
    pub rule: String,
    pub response: bool,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Bytes>,
    pub body_note: Option<&'static str>,
    pub timeout: Duration,
}

struct Entry {
    pending: Pending,
    held: Instant,
    deadline: Instant,
    reply: oneshot::Sender<Outcome>,
}

#[derive(Default)]
struct State {
    entries: BTreeMap<u64, Entry>,
    next: u64,
    revision: u64,
}

#[derive(Default)]
pub(crate) struct Queue {
    state: Mutex<State>,
}

impl Queue {
    /// Holds a message, or returns `None` when the queue is full.
    pub fn hold(&self, pending: Pending) -> Option<(u64, oneshot::Receiver<Outcome>)> {
        let mut state = self.state.lock().unwrap();
        if state.entries.len() >= crate::rules::HOLD_LIMIT {
            return None;
        }
        let (reply, outcome) = oneshot::channel();
        let id = state.next;
        state.next += 1;
        state.revision += 1;
        let held = Instant::now();
        state.entries.insert(
            id,
            Entry {
                deadline: held + pending.timeout,
                held,
                pending,
                reply,
            },
        );
        Some((id, outcome))
    }

    /// Resolves a held message. Returns false if it was no longer held.
    pub fn resolve(&self, id: u64, outcome: Outcome) -> bool {
        let entry = {
            let mut state = self.state.lock().unwrap();
            let entry = state.entries.remove(&id);
            state.revision += 1;
            entry
        };
        entry.is_some_and(|entry| entry.reply.send(outcome).is_ok())
    }

    /// Removes a message whose wait ended. Returns whether it was still held.
    pub fn cancel(&self, id: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        state.revision += 1;
        state.entries.remove(&id).is_some()
    }

    /// Lets every held message continue unmodified.
    pub fn release_all(&self, reason: &'static str) {
        let entries = {
            let mut state = self.state.lock().unwrap();
            state.revision += 1;
            std::mem::take(&mut state.entries)
        };
        for entry in entries.into_values() {
            let _ = entry.reply.send(Outcome::Release(reason));
        }
    }

    pub fn revision(&self) -> u64 {
        self.state.lock().unwrap().revision
    }

    pub fn len(&self) -> usize {
        self.state.lock().unwrap().entries.len()
    }

    /// Held messages, newest first, masked and without bodies.
    pub fn list(&self) -> Vec<HeldMessage> {
        let state = self.state.lock().unwrap();
        state
            .entries
            .iter()
            .rev()
            .map(|(id, entry)| view(*id, entry, false, false))
            .collect()
    }

    pub fn message(&self, id: u64, reveal: bool) -> Option<HeldMessage> {
        let state = self.state.lock().unwrap();
        state
            .entries
            .get(&id)
            .map(|entry| view(id, entry, reveal, true))
    }
}

fn view(id: u64, entry: &Entry, reveal: bool, body: bool) -> HeldMessage {
    let pending = &entry.pending;
    let secrets = if reveal { Secrets::Show } else { Secrets::Mask };
    let now = Instant::now();
    HeldMessage {
        id,
        flow: pending.flow,
        rule: pending.rule.clone(),
        response: pending.response,
        method: pending.method.clone(),
        url: presentation::url(&pending.url, secrets),
        status: pending.status,
        headers: presentation::headers(&pending.headers, secrets),
        body: body
            .then(|| {
                pending
                    .body
                    .as_ref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok().map(str::to_owned))
            })
            .flatten(),
        body_editable: pending.body.is_some(),
        body_note: pending.body_note.or_else(|| {
            pending
                .body
                .as_ref()
                .filter(|bytes| std::str::from_utf8(bytes).is_err())
                .map(|_| "Binary body is sent unchanged unless you enter text")
        }),
        held_for: now.saturating_duration_since(entry.held),
        remaining: entry.deadline.saturating_duration_since(now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(timeout: Duration) -> Pending {
        Pending {
            flow: Some(7),
            rule: "Hold".into(),
            response: false,
            method: "POST".into(),
            url: "http://x/a?token=secret&q=1".into(),
            status: None,
            headers: vec![
                ("authorization".into(), "Bearer secret".into()),
                ("x-plain".into(), "1".into()),
            ],
            body: Some(Bytes::from_static(b"{\"a\":1}")),
            body_note: None,
            timeout,
        }
    }

    #[tokio::test]
    async fn holds_resolve_once_and_list_newest_first_with_masking() {
        let queue = Queue::default();
        let (first, first_outcome) = queue.hold(pending(Duration::from_secs(60))).unwrap();
        let (second, second_outcome) = queue.hold(pending(Duration::from_secs(60))).unwrap();
        let list = queue.list();
        assert_eq!(
            list.iter().map(|m| m.id).collect::<Vec<_>>(),
            [second, first]
        );
        assert!(list[0].body.is_none() && list[0].body_editable);
        assert!(!list[0].url.contains("secret"));
        assert_eq!(list[0].headers[0].1, crate::MASK);
        let revealed = queue.message(first, true).unwrap();
        assert_eq!(revealed.headers[0].1, "Bearer secret");
        assert_eq!(revealed.body.as_deref(), Some("{\"a\":1}"));
        assert!(revealed.remaining <= Duration::from_secs(60));

        let revision = queue.revision();
        assert!(queue.resolve(
            first,
            Outcome::Execute(HeldEdits {
                body: Some("edited".into()),
                ..HeldEdits::default()
            })
        ));
        assert!(queue.revision() > revision);
        assert!(!queue.resolve(first, Outcome::Abort));
        assert!(matches!(
            first_outcome.await.unwrap(),
            Outcome::Execute(HeldEdits { body: Some(body), .. }) if body == "edited"
        ));
        assert!(queue.resolve(second, Outcome::Abort));
        assert!(matches!(second_outcome.await.unwrap(), Outcome::Abort));
        assert_eq!(queue.len(), 0);
    }

    #[tokio::test]
    async fn the_queue_is_capped_and_releases_everything_on_request() {
        let queue = Queue::default();
        let mut outcomes = Vec::new();
        for _ in 0..crate::rules::HOLD_LIMIT {
            outcomes.push(queue.hold(pending(Duration::from_secs(60))).unwrap());
        }
        assert!(queue.hold(pending(Duration::from_secs(60))).is_none());
        queue.release_all("Capture paused");
        assert_eq!(queue.len(), 0);
        for (_, outcome) in outcomes {
            assert!(matches!(
                outcome.await.unwrap(),
                Outcome::Release("Capture paused")
            ));
        }
        let (id, _outcome) = queue.hold(pending(Duration::ZERO)).unwrap();
        assert_eq!(queue.message(id, false).unwrap().remaining, Duration::ZERO);
        assert!(queue.cancel(id));
        assert!(!queue.cancel(id));
    }

    #[test]
    fn binary_bodies_are_editable_but_not_shown_as_text() {
        let queue = Queue::default();
        let (id, _outcome) = queue
            .hold(Pending {
                body: Some(Bytes::from_static(&[0xff, 0x00])),
                ..pending(Duration::from_secs(1))
            })
            .unwrap();
        let message = queue.message(id, false).unwrap();
        assert!(message.body.is_none() && message.body_editable);
        assert!(message.body_note.is_some());
    }
}
