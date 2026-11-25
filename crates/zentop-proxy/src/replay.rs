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

impl ReplayDraft {
    /// The starting point for a request composed from scratch.
    pub fn empty() -> Self {
        Self {
            method: "GET".into(),
            url: "http://".into(),
            headers: Vec::new(),
            body: Some(String::new()),
        }
    }

    pub(crate) fn size(&self) -> usize {
        self.method.len()
            + self.url.len()
            + self.body.as_ref().map_or(0, String::len)
            + self
                .headers
                .iter()
                .map(|(name, value)| name.len() + value.len() + 4)
                .sum::<usize>()
    }

    /// Builds a composed request. It goes through the same destination,
    /// decrypt-list and TLS checks as any replay.
    pub(crate) fn request(self) -> Result<http::Request<bytes::Bytes>, &'static str> {
        let uri: http::Uri = self
            .url
            .trim()
            .parse()
            .map_err(|_| "Enter an absolute http:// or https:// URL")?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
            return Err("Enter an absolute http:// or https:// URL");
        }
        let mut request = http::Request::builder()
            .method(self.method.trim())
            .uri(uri)
            .body(bytes::Bytes::from(self.body.unwrap_or_default()))
            .map_err(|_| "Enter a valid HTTP method")?;
        for (name, value) in self.headers {
            if name.trim().is_empty() {
                continue;
            }
            let name =
                http::HeaderName::try_from(name.trim()).map_err(|_| "Enter a valid header name")?;
            if name == http::header::PROXY_AUTHORIZATION || name == http::header::CONTENT_LENGTH {
                continue;
            }
            let value =
                http::HeaderValue::from_str(&value).map_err(|_| "Enter a valid header value")?;
            request.headers_mut().append(name, value);
        }
        Ok(request)
    }
}

/// Where a replayed request comes from.
#[derive(Debug)]
pub(crate) enum Source {
    Flow(u64, ReplayEdits),
    Composed(ReplayDraft),
}

pub(crate) fn supported(kind: FlowKind) -> bool {
    matches!(kind, FlowKind::Http | FlowKind::DecryptedHttps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_requests_need_absolute_http_urls_and_drop_proxy_credentials() {
        let draft = ReplayDraft::empty();
        assert!(draft.size() > 0);
        assert!(draft.request().is_err());
        for url in ["ftp://example.com/", "/relative", "example.com"] {
            let draft = ReplayDraft {
                url: url.into(),
                ..ReplayDraft::empty()
            };
            assert!(draft.request().is_err(), "{url}");
        }
        let draft = ReplayDraft {
            method: "POST".into(),
            url: " https://example.com/v1?q=1 ".into(),
            headers: vec![
                ("Proxy-Authorization".into(), "Basic x".into()),
                ("content-length".into(), "99".into()),
                ("x-test".into(), "1".into()),
                ("".into(), "ignored".into()),
            ],
            body: Some("{}".into()),
        };
        let request = draft.request().unwrap();
        assert_eq!(request.method(), http::Method::POST);
        assert_eq!(request.uri().to_string(), "https://example.com/v1?q=1");
        assert_eq!(request.headers().len(), 1);
        assert_eq!(request.body().as_ref(), b"{}");
        let bad = ReplayDraft {
            method: "BAD METHOD".into(),
            url: "http://example.com/".into(),
            ..ReplayDraft::empty()
        };
        assert!(bad.request().is_err());
    }
}
