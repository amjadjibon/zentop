use crate::presentation::{self, Secrets};
use crate::{Flow, FlowKind, Headers};

/// A header with an unchanged value can refer to its original captured row.
#[derive(Debug)]
pub struct ReplayHeader {
    pub name: String,
    pub value: ReplayValue,
}

#[derive(Debug)]
pub enum ReplayValue {
    Original(usize),
    Edited(String),
}

/// Fields left as None retain their original captured values.
#[derive(Debug, Default)]
pub struct ReplayEdits {
    pub method: Option<String>,
    pub url: Option<String>,
    pub preserve_masked_query: bool,
    pub headers: Option<Vec<ReplayHeader>>,
    pub body: Option<String>,
}

impl ReplayEdits {
    pub(crate) fn size(&self) -> usize {
        self.method.as_ref().map_or(0, String::len)
            + self.url.as_ref().map_or(0, String::len)
            + self.body.as_ref().map_or(0, String::len)
            + self.headers.as_ref().map_or(0, |headers| {
                headers
                    .iter()
                    .map(|h| {
                        h.name.len()
                            + 4
                            + match &h.value {
                                ReplayValue::Original(_) => 0,
                                ReplayValue::Edited(value) => value.len(),
                            }
                    })
                    .sum()
            })
    }
}

pub(crate) fn restore_query(original: &str, edited: String) -> String {
    let Some((_, original_query)) = original.split_once('?') else {
        return edited;
    };
    let Some((base, edited_query)) = edited.split_once('?') else {
        return edited;
    };
    let mut secrets = std::collections::HashMap::<&str, std::collections::VecDeque<&str>>::new();
    for pair in original_query.split('&') {
        if let Some((name, _)) = pair.split_once('=') {
            if presentation::secret_key(name) {
                secrets.entry(name).or_default().push_back(pair);
            }
        }
    }

    let mut result = base.to_owned();
    let mut separator = '?';
    for pair in edited_query.split('&') {
        result.push(separator);
        separator = '&';
        let original = pair.split_once('=').and_then(|(name, value)| {
            let original = secrets.get_mut(name)?.pop_front();
            original.filter(|_| value == presentation::MASK)
        });
        result.push_str(original.unwrap_or(pair));
    }
    result
}

#[derive(Debug)]
pub struct ReplayDraft {
    pub method: String,
    pub url: String,
    pub headers: Headers,
    /// None when the retained bytes aren't valid UTF-8. Leaving them untouched
    /// still replays the original bytes.
    pub body: Option<String>,
}

impl ReplayDraft {
    pub(crate) fn from_flow(flow: Flow, reveal: bool) -> Self {
        Self {
            method: flow.request.method,
            url: if reveal {
                flow.request.url
            } else {
                presentation::url(&flow.request.url, Secrets::Mask)
            },
            headers: flow.request.headers,
            body: std::str::from_utf8(flow.request_body.kept())
                .ok()
                .map(str::to_owned),
        }
    }
}

pub(crate) fn supported(kind: FlowKind) -> bool {
    matches!(kind, FlowKind::Http | FlowKind::DecryptedHttps)
}
