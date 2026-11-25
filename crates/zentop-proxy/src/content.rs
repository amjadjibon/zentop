//! Content kinds for the Traffic window's tabs. The store classifies each flow
//! as its heads and request body arrive, so views never inspect flows per frame.
use crate::FlowKind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContentKind {
    Json,
    Form,
    Xml,
    Js,
    Css,
    GraphQl,
    Document,
    Media,
    #[default]
    Other,
}

impl ContentKind {
    pub const ALL: [Self; 9] = [
        Self::Json,
        Self::Form,
        Self::Xml,
        Self::Js,
        Self::Css,
        Self::GraphQl,
        Self::Document,
        Self::Media,
        Self::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Form => "Form",
            Self::Xml => "XML",
            Self::Js => "JS",
            Self::Css => "CSS",
            Self::GraphQl => "GraphQL",
            Self::Document => "Document",
            Self::Media => "Media",
            Self::Other => "Other",
        }
    }

    /// Maps a `Content-Type` value to a kind; `None` for types with no tab.
    pub fn from_mime(value: &str) -> Option<Self> {
        let essence = value.split(';').next()?.trim().to_ascii_lowercase();
        let (top, sub) = essence.split_once('/')?;
        Some(match (top, sub) {
            ("image" | "audio" | "video" | "font", _) => Self::Media,
            ("application", "vnd.ms-fontobject" | "font-woff" | "x-font-ttf" | "x-font-woff") => {
                Self::Media
            }
            ("text", "html") | ("application", "xhtml+xml") => Self::Document,
            ("application", "graphql") => Self::GraphQl,
            ("application", "x-www-form-urlencoded") | ("multipart", "form-data") => Self::Form,
            ("text", "css") => Self::Css,
            (
                "application" | "text",
                "javascript" | "x-javascript" | "ecmascript" | "x-ecmascript",
            ) => Self::Js,
            ("application" | "text", "json") | ("application", "x-ndjson") => Self::Json,
            (_, sub) if sub.ends_with("+json") => Self::Json,
            ("application" | "text", "xml") => Self::Xml,
            (_, sub) if sub.ends_with("+xml") => Self::Xml,
            _ => return None,
        })
    }
}

/// What a flow has revealed so far about its content.
#[derive(Clone, Debug, Default)]
pub(crate) struct Classifier {
    request: Option<ContentKind>,
    response: Option<ContentKind>,
    graphql: bool,
}

impl Classifier {
    pub(crate) fn new(path: &str, request_type: Option<&str>) -> Self {
        let request = request_type.and_then(ContentKind::from_mime);
        let path = path.trim_end_matches('/');
        let graphql = request == Some(ContentKind::GraphQl)
            || path
                .rsplit('/')
                .next()
                .is_some_and(|last| last.eq_ignore_ascii_case("graphql"));
        Self {
            request,
            response: None,
            graphql,
        }
    }

    pub(crate) fn response(&mut self, content_type: Option<&str>) {
        self.response = content_type.and_then(ContentKind::from_mime);
    }

    /// Checks a complete JSON request body once for a GraphQL operation.
    pub(crate) fn request_body(&mut self, body: &[u8]) {
        if self.graphql || self.request != Some(ContentKind::Json) || body.is_empty() {
            return;
        }
        // Bodies are capped at 1 MB, and this runs once when the request ends.
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
            return;
        };
        // A batch is an array of operations.
        let operation = match &value {
            serde_json::Value::Array(operations) => operations.first(),
            operation => Some(operation),
        };
        self.graphql = operation
            .and_then(|operation| operation.get("query"))
            .is_some_and(serde_json::Value::is_string);
    }

    pub(crate) fn kind(&self, flow: FlowKind) -> ContentKind {
        if matches!(flow, FlowKind::Tunnel | FlowKind::Upgrade) {
            return ContentKind::Other;
        }
        if self.graphql {
            return ContentKind::GraphQl;
        }
        if self.request == Some(ContentKind::Form) {
            return ContentKind::Form;
        }
        self.response.or(self.request).unwrap_or(ContentKind::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_content_types_to_kinds() {
        for (mime, kind) in [
            ("application/json; charset=utf-8", Some(ContentKind::Json)),
            ("application/problem+json", Some(ContentKind::Json)),
            ("application/x-www-form-urlencoded", Some(ContentKind::Form)),
            ("multipart/form-data; boundary=x", Some(ContentKind::Form)),
            ("text/xml", Some(ContentKind::Xml)),
            ("application/atom+xml", Some(ContentKind::Xml)),
            ("image/svg+xml", Some(ContentKind::Media)),
            ("application/xhtml+xml", Some(ContentKind::Document)),
            ("text/html", Some(ContentKind::Document)),
            ("text/javascript", Some(ContentKind::Js)),
            ("TEXT/CSS", Some(ContentKind::Css)),
            ("application/graphql", Some(ContentKind::GraphQl)),
            ("image/png", Some(ContentKind::Media)),
            ("font/woff2", Some(ContentKind::Media)),
            ("video/mp4", Some(ContentKind::Media)),
            ("text/plain", None),
            ("application/octet-stream", None),
            ("garbage", None),
        ] {
            assert_eq!(ContentKind::from_mime(mime), kind, "{mime}");
        }
    }

    #[test]
    fn detects_graphql_by_path_type_or_json_query() {
        let by_path = Classifier::new("/api/GraphQL/", None);
        assert_eq!(by_path.kind(FlowKind::Http), ContentKind::GraphQl);
        let by_type = Classifier::new("/q", Some("application/graphql"));
        assert_eq!(by_type.kind(FlowKind::Http), ContentKind::GraphQl);

        let mut body = Classifier::new("/api", Some("application/json"));
        body.request_body(br#"{"query":"{ viewer { id } }","variables":{}}"#);
        assert_eq!(body.kind(FlowKind::DecryptedHttps), ContentKind::GraphQl);
        let mut batch = Classifier::new("/api", Some("application/json"));
        batch.request_body(br#"[{"query":"{ a }"}]"#);
        assert_eq!(batch.kind(FlowKind::Http), ContentKind::GraphQl);

        let mut not_string = Classifier::new("/api", Some("application/json"));
        not_string.request_body(br#"{"query":{"id":1}}"#);
        not_string.response(Some("application/json"));
        assert_eq!(not_string.kind(FlowKind::Http), ContentKind::Json);
        let mut nested = Classifier::new("/api", Some("application/json"));
        nested.request_body(br#"{"data":{"query":"x"}}"#);
        assert_eq!(nested.kind(FlowKind::Http), ContentKind::Json);
        let mut plain_text = Classifier::new("/api", Some("text/plain"));
        plain_text.request_body(br#"{"query":"x"}"#);
        assert_eq!(plain_text.kind(FlowKind::Http), ContentKind::Other);
        assert!(!Classifier::new("/graphqlx", None).graphql);
    }

    #[test]
    fn forms_win_over_responses_and_tunnels_are_other() {
        let mut form = Classifier::new("/login", Some("application/x-www-form-urlencoded"));
        form.response(Some("text/html"));
        assert_eq!(form.kind(FlowKind::Http), ContentKind::Form);

        let mut page = Classifier::new("/", None);
        assert_eq!(page.kind(FlowKind::Http), ContentKind::Other);
        page.response(Some("text/html; charset=utf-8"));
        assert_eq!(page.kind(FlowKind::Http), ContentKind::Document);
        page.response(Some("image/webp"));
        assert_eq!(page.kind(FlowKind::Http), ContentKind::Media);

        let mut json_request = Classifier::new("/upload", Some("application/json"));
        assert_eq!(json_request.kind(FlowKind::Http), ContentKind::Json);
        json_request.response(Some("text/plain"));
        assert_eq!(json_request.kind(FlowKind::Http), ContentKind::Json);

        let graphql = Classifier::new("/graphql", None);
        assert_eq!(graphql.kind(FlowKind::Tunnel), ContentKind::Other);
        assert_eq!(graphql.kind(FlowKind::Upgrade), ContentKind::Other);
    }
}
