//! Live-editing rules: breakpoints, Map Local, Map Remote and rewrites.
//!
//! Rules are written by the user in the GUI and apply only to traffic Zentop
//! sees in plaintext. This module validates and matches them and implements
//! each rewrite as a pure function, so the engine only sequences them.
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use http::header::{self, HeaderName, HeaderValue};
use http::{HeaderMap, StatusCode, Uri};
use serde::{Deserialize, Serialize};

/// The most rules `settings.json` keeps.
pub const RULE_LIMIT: usize = 200;
/// The most messages held at breakpoints at once.
pub const HOLD_LIMIT: usize = 32;
/// The largest body read for editing at a breakpoint or by a rewrite.
pub const EDIT_BODY_LIMIT: usize = 1024 * 1024;
/// How long a message stays held before it continues unmodified.
pub const HOLD_TIMEOUT: Duration = Duration::from_secs(120);
/// The largest file Map Local serves.
pub const MAP_LOCAL_LIMIT: u64 = 10 * 1024 * 1024;
const REWRITTEN_BODY_LIMIT: usize = 4 * 1024 * 1024;
const ACTION_LIMIT: usize = 32;
const NAME_LIMIT: usize = 100;
const PATTERN_LIMIT: usize = 2048;
const VALUE_LIMIT: usize = 8 * 1024;
const REGEX_LIMIT: usize = 1024;

/// Which message a rewrite action edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleSide {
    Request,
    Response,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleKind {
    Breakpoint,
    MapLocal,
    MapRemote,
    Rewrite,
}

impl RuleKind {
    pub const ALL: [Self; 4] = [
        Self::Breakpoint,
        Self::MapLocal,
        Self::MapRemote,
        Self::Rewrite,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Breakpoint => "Breakpoint",
            Self::MapLocal => "Map Local",
            Self::MapRemote => "Map Remote",
            Self::Rewrite => "Rewrite",
        }
    }
}

/// One user-authored rule, as saved in the GUI settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    /// One method token, or any method when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// `*` wildcards over `scheme://host[:port]/path?query`.
    pub url: String,
    /// The attributed process name, compared without regard to case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    Breakpoint {
        request: bool,
        response: bool,
    },
    #[serde(rename_all = "camelCase")]
    MapLocal {
        path: String,
        #[serde(default = "ok_status")]
        status: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        headers: Vec<(String, String)>,
    },
    #[serde(rename_all = "camelCase")]
    MapRemote {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheme: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default = "yes")]
        keep_query: bool,
    },
    Rewrite {
        actions: Vec<RewriteAction>,
    },
}

fn ok_status() -> u16 {
    200
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RewriteAction {
    /// Appends a header value.
    AddHeader {
        side: RuleSide,
        name: String,
        value: String,
    },
    /// Replaces every value of a header with one value.
    SetHeader {
        side: RuleSide,
        name: String,
        value: String,
    },
    RemoveHeader {
        side: RuleSide,
        name: String,
    },
    /// Sets the response status code.
    Status {
        code: u16,
    },
    /// Replaces text in a body, literally or with a regex.
    ReplaceBody {
        side: RuleSide,
        find: String,
        replace: String,
        #[serde(default)]
        regex: bool,
    },
    /// Adds or replaces a request query parameter.
    SetQuery {
        name: String,
        value: String,
    },
    RemoveQuery {
        name: String,
    },
}

impl RewriteAction {
    pub fn side(&self) -> RuleSide {
        match self {
            Self::AddHeader { side, .. }
            | Self::SetHeader { side, .. }
            | Self::RemoveHeader { side, .. }
            | Self::ReplaceBody { side, .. } => *side,
            Self::Status { .. } => RuleSide::Response,
            Self::SetQuery { .. } | Self::RemoveQuery { .. } => RuleSide::Request,
        }
    }

    fn validate(&self) -> Result<(), String> {
        let header_name = |name: &str| -> Result<(), String> {
            let parsed = HeaderName::from_bytes(name.trim().as_bytes())
                .map_err(|_| format!("“{name}” is not a valid header name"))?;
            if RESERVED_HEADERS.contains(&parsed.as_str()) {
                return Err(format!(
                    "Zentop manages the {} header; rules can't change it",
                    parsed.as_str()
                ));
            }
            Ok(())
        };
        let header_value = |value: &str| -> Result<(), String> {
            if value.len() > VALUE_LIMIT || HeaderValue::from_str(value).is_err() {
                return Err("Enter a header value of at most 8 KB without line breaks".into());
            }
            Ok(())
        };
        match self {
            Self::AddHeader { name, value, .. } | Self::SetHeader { name, value, .. } => {
                header_name(name)?;
                header_value(value)
            }
            Self::RemoveHeader { name, .. } => header_name(name),
            Self::Status { code } => valid_status(*code),
            Self::ReplaceBody {
                find,
                replace,
                regex,
                ..
            } => {
                if find.is_empty() {
                    return Err("Enter the text to find in the body".into());
                }
                if replace.len() > VALUE_LIMIT || find.len() > VALUE_LIMIT {
                    return Err("Body find and replace text are limited to 8 KB".into());
                }
                if *regex {
                    compile(find)?;
                }
                Ok(())
            }
            Self::SetQuery { name, value } => {
                query_name(name)?;
                if value.len() > VALUE_LIMIT || value.chars().any(char::is_control) {
                    return Err("Enter a query value of at most 8 KB".into());
                }
                Ok(())
            }
            Self::RemoveQuery { name } => query_name(name),
        }
    }
}

/// Headers whose framing or routing Zentop owns.
const RESERVED_HEADERS: [&str; 6] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
    "upgrade",
];

fn query_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err("Enter a query parameter name of at most 256 characters".into());
    }
    Ok(())
}

fn valid_status(code: u16) -> Result<(), String> {
    if (200..=599).contains(&code) {
        Ok(())
    } else {
        Err("Enter a status code from 200 to 599".into())
    }
}

fn compile(pattern: &str) -> Result<regex::Regex, String> {
    if pattern.len() > REGEX_LIMIT {
        return Err("Regular expressions are limited to 1 KB".into());
    }
    regex::RegexBuilder::new(pattern)
        .size_limit(256 * 1024)
        .dfa_size_limit(1024 * 1024)
        .build()
        .map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => "The regular expression is too complex".into(),
            error => format!("Invalid regular expression: {error}"),
        })
}

fn plain_text(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}

impl Rule {
    pub fn kind(&self) -> RuleKind {
        match self.action {
            Action::Breakpoint { .. } => RuleKind::Breakpoint,
            Action::MapLocal { .. } => RuleKind::MapLocal,
            Action::MapRemote { .. } => RuleKind::MapRemote,
            Action::Rewrite { .. } => RuleKind::Rewrite,
        }
    }

    /// Trims fields and checks every limit. The GUI saves only rules that pass.
    pub fn normalize(mut self) -> Result<Self, String> {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() || !plain_text(&self.name, NAME_LIMIT) {
            return Err("Enter a rule name of at most 100 characters".into());
        }
        self.method = self
            .method
            .map(|method| method.trim().to_ascii_uppercase())
            .filter(|method| !method.is_empty() && method != "ANY" && method != "*");
        if let Some(method) = &self.method {
            if method.len() > 32 || http::Method::from_bytes(method.as_bytes()).is_err() {
                return Err("Enter one HTTP method, or leave it blank for any".into());
            }
        }
        self.url = self.url.trim().to_owned();
        Pattern::new(&self.url)?;
        self.app = self
            .app
            .map(|app| app.trim().to_owned())
            .filter(|app| !app.is_empty());
        if self.app.as_ref().is_some_and(|app| !plain_text(app, 255)) {
            return Err("Enter an app name of at most 255 characters".into());
        }
        match &mut self.action {
            Action::Breakpoint { request, response } => {
                if !*request && !*response {
                    return Err("Hold requests, responses or both".into());
                }
            }
            Action::MapLocal {
                path,
                status,
                content_type,
                headers,
            } => {
                *path = path.trim().to_owned();
                if !Path::new(path.as_str()).is_absolute() || !plain_text(path, 4096) {
                    return Err("Choose a file by its full path".into());
                }
                valid_status(*status)?;
                *content_type = content_type
                    .take()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty());
                if content_type
                    .as_ref()
                    .is_some_and(|value| value.len() > 256 || HeaderValue::from_str(value).is_err())
                {
                    return Err("Enter a valid content type".into());
                }
                if headers.len() > ACTION_LIMIT {
                    return Err("Map Local supports up to 32 extra headers".into());
                }
                for (name, value) in headers.iter_mut() {
                    *name = name.trim().to_owned();
                    RewriteAction::SetHeader {
                        side: RuleSide::Response,
                        name: name.clone(),
                        value: value.clone(),
                    }
                    .validate()?;
                }
            }
            Action::MapRemote {
                scheme,
                host,
                port,
                path,
                ..
            } => {
                let trim = |value: &mut Option<String>| {
                    *value = value
                        .take()
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty());
                };
                trim(scheme);
                trim(host);
                trim(path);
                if let Some(value) = scheme {
                    *value = value.to_ascii_lowercase();
                    if value != "http" && value != "https" {
                        return Err("Map Remote supports http and https".into());
                    }
                }
                if let Some(value) = host {
                    *value = destination_host(value)?;
                }
                if *port == Some(0) {
                    return Err("Enter a port from 1 to 65535".into());
                }
                if let Some(value) = path {
                    if !value.starts_with('/')
                        || value.contains(['?', '#'])
                        || value.len() > PATTERN_LIMIT
                        || value.parse::<http::uri::PathAndQuery>().is_err()
                    {
                        return Err("Enter a path that starts with / and has no query".into());
                    }
                }
                if scheme.is_none() && host.is_none() && port.is_none() && path.is_none() {
                    return Err("Change at least the scheme, host, port or path".into());
                }
            }
            Action::Rewrite { actions } => {
                if actions.is_empty() || actions.len() > ACTION_LIMIT {
                    return Err("A rewrite rule needs 1 to 32 actions".into());
                }
                for action in actions.iter_mut() {
                    if let RewriteAction::AddHeader { name, .. }
                    | RewriteAction::SetHeader { name, .. }
                    | RewriteAction::RemoveHeader { name, .. } = action
                    {
                        *name = name.trim().to_ascii_lowercase();
                    }
                    action.validate()?;
                }
            }
        }
        Ok(self)
    }
}

/// Canonicalizes a Map Remote host: a DNS name, IPv4 address or bracketed IPv6.
fn destination_host(raw: &str) -> Result<String, String> {
    let error = || "Enter a host name or IP address without a port".to_owned();
    if let Some(inner) = raw.strip_prefix('[').and_then(|ip| ip.strip_suffix(']')) {
        return inner
            .parse::<std::net::Ipv6Addr>()
            .map(|ip| format!("[{ip}]"))
            .map_err(|_| error());
    }
    if let Ok(ip) = raw.parse::<std::net::Ipv4Addr>() {
        return Ok(ip.to_string());
    }
    if raw == "localhost" || raw.eq_ignore_ascii_case("localhost.") {
        return Ok("localhost".into());
    }
    crate::canonical_host(raw).map_err(|_| error())
}

/// Reads saved rules, dropping invalid ones and duplicate IDs, up to the cap.
pub fn load_rules(value: &serde_json::Value) -> Vec<Rule> {
    let mut seen = std::collections::HashSet::new();
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rule| serde_json::from_value::<Rule>(rule.clone()).ok())
        .filter_map(|rule| rule.normalize().ok())
        .filter(|rule| seen.insert(rule.id))
        .take(RULE_LIMIT)
        .collect()
}

/// A URL pattern prefilled from a flow: its URL without the query.
pub fn pattern_from_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    match base.parse::<Uri>() {
        Ok(uri) if uri.scheme().is_some() && uri.authority().is_some() => {
            let scheme = uri.scheme_str().unwrap_or("http").to_ascii_lowercase();
            let authority = uri.authority().unwrap();
            let host = authority.host().to_ascii_lowercase();
            let port = authority
                .port_u16()
                .filter(|port| Some(*port) != default_port(&scheme));
            format!(
                "{scheme}://{host}{}{}",
                port.map(|port| format!(":{port}")).unwrap_or_default(),
                uri.path()
            )
        }
        _ => base.to_owned(),
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// The canonical URL rules match: lowercase scheme and host, the default port
/// omitted, then the path and query exactly as sent.
pub(crate) fn rule_url(scheme: &str, host: &str, port: u16, path_and_query: &str) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = if Some(port) == default_port(scheme) {
        String::new()
    } else {
        format!(":{port}")
    };
    let path = if path_and_query.starts_with('/') {
        path_and_query
    } else {
        "/"
    };
    format!("{scheme}://{host}{port}{path}")
}

/// A compiled URL pattern.
#[derive(Clone, Debug)]
pub(crate) struct Pattern {
    glob: Vec<u8>,
    query: bool,
}

impl Pattern {
    pub(crate) fn new(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty()
            || raw.len() > PATTERN_LIMIT
            || raw.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("Enter a URL pattern of at most 2 KB without spaces".into());
        }
        let (scheme, rest) = match raw.split_once("://") {
            Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
            None => ("*".to_owned(), raw),
        };
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(split);
        if authority.is_empty() {
            return Err("Enter a host or * after the scheme".into());
        }
        let mut authority = authority.to_ascii_lowercase();
        if let Some(port) = default_port(&scheme) {
            if let Some(host) = authority.strip_suffix(&format!(":{port}")) {
                authority = host.to_owned();
            }
        }
        let tail = if tail.is_empty() {
            "/*".to_owned()
        } else if tail.starts_with('?') {
            format!("/{tail}")
        } else {
            tail.to_owned()
        };
        Ok(Self {
            query: tail.contains('?'),
            glob: format!("{scheme}://{authority}{tail}").into_bytes(),
        })
    }

    pub(crate) fn matches(&self, url: &str) -> bool {
        let url = if self.query {
            url
        } else {
            url.split('?').next().unwrap_or(url)
        };
        glob(&self.glob, url.as_bytes())
    }
}

/// `*` matches any run of bytes, including none; every other byte is literal.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star = None;
    let mut resume = 0;
    while t < text.len() {
        if pattern.get(p) == Some(&b'*') {
            star = Some(p);
            p += 1;
            resume = t;
        } else if pattern.get(p) == Some(&text[t]) {
            p += 1;
            t += 1;
        } else if let Some(position) = star {
            p = position + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}

/// A validated rule ready to run.
#[derive(Debug)]
pub(crate) struct Compiled {
    pub rule: Rule,
    pattern: Pattern,
    regexes: Vec<Option<regex::Regex>>,
}

impl Compiled {
    fn new(rule: Rule) -> Result<Self, String> {
        let rule = rule.normalize()?;
        let pattern = Pattern::new(&rule.url)?;
        let regexes = match &rule.action {
            Action::Rewrite { actions } => actions
                .iter()
                .map(|action| match action {
                    RewriteAction::ReplaceBody {
                        find, regex: true, ..
                    } => compile(find).map(Some),
                    _ => Ok(None),
                })
                .collect::<Result<_, _>>()?,
            _ => Vec::new(),
        };
        Ok(Self {
            rule,
            pattern,
            regexes,
        })
    }

    fn matches(&self, method: &str, url: &str, app: Option<&str>) -> bool {
        self.rule.enabled
            && self
                .rule
                .method
                .as_ref()
                .is_none_or(|wanted| wanted == method)
            && self.rule.app.as_ref().is_none_or(|wanted| {
                app.is_some_and(|app| app.to_lowercase() == wanted.to_lowercase())
            })
            && self.pattern.matches(url)
    }

    pub fn actions(&self) -> &[RewriteAction] {
        match &self.rule.action {
            Action::Rewrite { actions } => actions,
            _ => &[],
        }
    }

    pub fn touches(&self, side: RuleSide) -> bool {
        self.actions().iter().any(|action| action.side() == side)
    }

    fn body_actions(
        &self,
        side: RuleSide,
    ) -> impl Iterator<Item = (&RewriteAction, Option<&regex::Regex>)> {
        self.actions()
            .iter()
            .zip(&self.regexes)
            .filter(move |(action, _)| {
                action.side() == side && matches!(action, RewriteAction::ReplaceBody { .. })
            })
            .map(|(action, regex)| (action, regex.as_ref()))
    }

    pub fn edits_body(&self, side: RuleSide) -> bool {
        self.body_actions(side).next().is_some()
    }
}

/// The rules a running session applies, compiled once per change.
#[derive(Debug)]
pub struct RuleSet {
    rules: Vec<Arc<Compiled>>,
    breakpoints: bool,
    pub(crate) hold_timeout: Duration,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            breakpoints: true,
            hold_timeout: HOLD_TIMEOUT,
        }
    }
}

impl RuleSet {
    /// Compiles rules in order. `breakpoints` is the master switch.
    pub fn new(rules: Vec<Rule>, breakpoints: bool) -> Result<Self, String> {
        if rules.len() > RULE_LIMIT {
            return Err("Zentop keeps at most 200 rules".into());
        }
        Ok(Self {
            rules: rules
                .into_iter()
                .map(|rule| Compiled::new(rule).map(Arc::new))
                .collect::<Result<_, _>>()?,
            breakpoints,
            hold_timeout: HOLD_TIMEOUT,
        })
    }

    /// Shortens the hold timeout, for tests and demonstrations.
    pub fn with_hold_timeout(mut self, timeout: Duration) -> Self {
        self.hold_timeout = timeout;
        self
    }

    pub fn breakpoints_enabled(&self) -> bool {
        self.breakpoints
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rules.iter().all(|rule| !rule.rule.enabled)
    }

    pub(crate) fn plan(&self, method: &str, url: &str, app: Option<&str>) -> Plan {
        let mut plan = Plan::default();
        for rule in self
            .rules
            .iter()
            .filter(|rule| rule.matches(method, url, app))
        {
            match &rule.rule.action {
                Action::MapLocal { .. } | Action::MapRemote { .. } => {
                    plan.mapping.get_or_insert_with(|| rule.clone());
                }
                Action::Rewrite { .. } => plan.rewrites.push(rule.clone()),
                Action::Breakpoint { request, response } if self.breakpoints => {
                    if *request {
                        plan.request_break.get_or_insert_with(|| rule.clone());
                    }
                    if *response {
                        plan.response_break.get_or_insert_with(|| rule.clone());
                    }
                }
                Action::Breakpoint { .. } => {}
            }
        }
        plan
    }
}

/// What the rules decided for one request.
#[derive(Default)]
pub(crate) struct Plan {
    pub mapping: Option<Arc<Compiled>>,
    pub rewrites: Vec<Arc<Compiled>>,
    pub request_break: Option<Arc<Compiled>>,
    pub response_break: Option<Arc<Compiled>>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.mapping.is_none()
            && self.rewrites.is_empty()
            && self.request_break.is_none()
            && self.response_break.is_none()
    }
}

/// Applies header actions for one side, in order. Returns how many applied.
pub(crate) fn rewrite_headers(rule: &Compiled, side: RuleSide, headers: &mut HeaderMap) -> usize {
    let mut applied = 0;
    for action in rule.actions().iter().filter(|action| action.side() == side) {
        let parse = |name: &str, value: Option<&str>| {
            let name = HeaderName::from_bytes(name.as_bytes()).ok()?;
            let value = match value {
                Some(value) => Some(HeaderValue::from_str(value).ok()?),
                None => None,
            };
            Some((name, value))
        };
        match action {
            RewriteAction::AddHeader { name, value, .. } => {
                if let Some((name, Some(value))) = parse(name, Some(value)) {
                    headers.append(name, value);
                    applied += 1;
                }
            }
            RewriteAction::SetHeader { name, value, .. } => {
                if let Some((name, Some(value))) = parse(name, Some(value)) {
                    headers.insert(name, value);
                    applied += 1;
                }
            }
            RewriteAction::RemoveHeader { name, .. } => {
                if let Some((name, _)) = parse(name, None) {
                    if headers.remove(name).is_some() {
                        applied += 1;
                    }
                }
            }
            _ => {}
        }
    }
    applied
}

/// Applies a status action. Returns whether the status changed.
pub(crate) fn rewrite_status(rule: &Compiled, status: &mut StatusCode) -> bool {
    let mut changed = false;
    for action in rule.actions() {
        if let RewriteAction::Status { code } = action {
            if let Ok(code) = StatusCode::from_u16(*code) {
                changed |= *status != code;
                *status = code;
            }
        }
    }
    changed
}

/// Applies query actions to a request URI. Returns the new URI when it changed.
pub(crate) fn rewrite_query(rule: &Compiled, uri: &Uri) -> Option<Uri> {
    let mut pairs: Vec<String> = uri
        .query()
        .map(|query| {
            query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let name_of = |pair: &str| percent_decode(pair.split('=').next().unwrap_or(pair));
    let mut changed = false;
    for action in rule.actions() {
        match action {
            RewriteAction::SetQuery { name, value } => {
                let pair = format!("{}={}", percent_encode(name), percent_encode(value));
                match pairs.iter().position(|pair| name_of(pair) == *name) {
                    Some(index) => {
                        pairs[index] = pair;
                        let mut index = index + 1;
                        while index < pairs.len() {
                            if name_of(&pairs[index]) == *name {
                                pairs.remove(index);
                            } else {
                                index += 1;
                            }
                        }
                    }
                    None => pairs.push(pair),
                }
                changed = true;
            }
            RewriteAction::RemoveQuery { name } => {
                let before = pairs.len();
                pairs.retain(|pair| name_of(pair) != *name);
                changed |= pairs.len() != before;
            }
            _ => {}
        }
    }
    if !changed {
        return None;
    }
    let mut parts = uri.clone().into_parts();
    let path = uri.path();
    let path_and_query = if pairs.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", pairs.join("&"))
    };
    parts.path_and_query = Some(path_and_query.parse().ok()?);
    Uri::from_parts(parts).ok()
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    decoded.push((high * 16 + low) as u8);
                    index += 3;
                    continue;
                }
                decoded.push(b'%');
            }
            b'+' => decoded.push(b' '),
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// The outcome of a body rewrite.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BodyEdit {
    Unchanged,
    Changed(Vec<u8>),
    Skipped(&'static str),
}

/// Decodes `Content-Encoding` into at most [`EDIT_BODY_LIMIT`] bytes.
pub(crate) fn decode(headers: &HeaderMap, body: &[u8]) -> Result<(Vec<u8>, bool), &'static str> {
    let encodings: Vec<String> = headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "identity")
        .collect();
    if encodings.is_empty() || body.is_empty() {
        return Ok((body.to_vec(), false));
    }
    let mut source: Box<dyn Read + '_> = Box::new(body);
    for encoding in encodings.iter().rev() {
        source = match encoding.as_str() {
            "gzip" | "x-gzip" => Box::new(flate2::read::MultiGzDecoder::new(source)),
            "br" => Box::new(brotli::Decompressor::new(source, 4096)),
            "deflate" => Box::new(flate2::read::ZlibDecoder::new(source)),
            _ => return Err("the body's content encoding is not supported"),
        };
    }
    let mut decoded = Vec::new();
    source
        .take(EDIT_BODY_LIMIT as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| "the body could not be decompressed")?;
    if decoded.len() > EDIT_BODY_LIMIT {
        return Err("the decompressed body is over 1 MB");
    }
    Ok((decoded, true))
}

/// Applies body actions for one side. A changed, previously compressed body is
/// returned decoded, and `Content-Encoding` is removed from `headers`.
pub(crate) fn rewrite_body(
    rule: &Compiled,
    side: RuleSide,
    headers: &mut HeaderMap,
    body: &[u8],
) -> BodyEdit {
    if !rule.edits_body(side) {
        return BodyEdit::Unchanged;
    }
    if body.len() > EDIT_BODY_LIMIT {
        return BodyEdit::Skipped("Body skipped: over 1 MB");
    }
    let (decoded, encoded) = match decode(headers, body) {
        Ok(decoded) => decoded,
        Err(_) => return BodyEdit::Skipped("Body skipped: could not decode its content encoding"),
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return BodyEdit::Skipped("Body skipped: not text");
    };
    let mut current = text.clone();
    for (action, regex) in rule.body_actions(side) {
        let RewriteAction::ReplaceBody { find, replace, .. } = action else {
            continue;
        };
        current = match regex {
            Some(regex) => regex.replace_all(&current, replace.as_str()).into_owned(),
            None => current.replace(find.as_str(), replace),
        };
        if current.len() > REWRITTEN_BODY_LIMIT {
            return BodyEdit::Skipped("Body skipped: the result would exceed 4 MB");
        }
    }
    if current == text {
        return BodyEdit::Unchanged;
    }
    if encoded {
        headers.remove(header::CONTENT_ENCODING);
    }
    BodyEdit::Changed(current.into_bytes())
}

/// Removes the framing headers so Hyper frames a replaced body exactly.
pub(crate) fn reframe(headers: &mut HeaderMap) {
    headers.remove(header::CONTENT_LENGTH);
    headers.remove(header::TRANSFER_ENCODING);
}

/// Reads a Map Local file, refusing anything but a regular file within the cap.
pub(crate) fn read_local(path: &str, limit: u64) -> Result<Vec<u8>, &'static str> {
    let describe = |error: std::io::Error| match error.kind() {
        std::io::ErrorKind::NotFound => "Map Local file not found",
        std::io::ErrorKind::PermissionDenied => "Map Local file is not readable",
        _ => "Map Local file could not be read",
    };
    // Checked before opening, so a FIFO or device is never opened.
    let metadata = std::fs::metadata(path).map_err(describe)?;
    if !metadata.is_file() {
        return Err("Map Local path is not a regular file");
    }
    if metadata.len() > limit {
        return Err("Map Local file is larger than 10 MB");
    }
    let file = std::fs::File::open(path).map_err(describe)?;
    if !file.metadata().map_err(describe)?.is_file() {
        return Err("Map Local path is not a regular file");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(describe)?;
    if bytes.len() as u64 > limit {
        return Err("Map Local file is larger than 10 MB");
    }
    Ok(bytes)
}

/// A content type guessed from a Map Local file's extension.
pub(crate) fn content_type_for(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("txt" | "log") => "text/plain; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Builds the response head Map Local serves for `len` bytes.
pub(crate) fn local_head(rule: &Rule, len: usize) -> Option<http::response::Parts> {
    let Action::MapLocal {
        path,
        status,
        content_type,
        headers,
    } = &rule.action
    else {
        return None;
    };
    let mut response = http::Response::new(());
    *response.status_mut() = StatusCode::from_u16(*status).ok()?;
    let content_type = content_type
        .as_deref()
        .unwrap_or_else(|| content_type_for(path));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).ok()?,
    );
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            response.headers_mut().append(name, value);
        }
    }
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    Some(response.into_parts().0)
}

/// The absolute URL a Map Remote rule sends a request to.
pub(crate) fn remote_url(
    rule: &Rule,
    scheme: &str,
    host: &str,
    port: u16,
    path_and_query: &str,
) -> Option<String> {
    let Action::MapRemote {
        scheme: new_scheme,
        host: new_host,
        port: new_port,
        path: new_path,
        keep_query,
    } = &rule.action
    else {
        return None;
    };
    let original_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let scheme_out = new_scheme.as_deref().unwrap_or(scheme);
    let host_out = new_host.clone().unwrap_or(original_host.clone());
    let port_out = new_port.unwrap_or_else(|| {
        if scheme_out == scheme && host_out == original_host {
            port
        } else {
            default_port(scheme_out).unwrap_or(80)
        }
    });
    let (path, query) = match path_and_query.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path_and_query, None),
    };
    let path = new_path
        .as_deref()
        .unwrap_or(if path.is_empty() { "/" } else { path });
    let query = query
        .filter(|_| *keep_query)
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    Some(format!(
        "{}{query}",
        rule_url(
            scheme_out,
            host_out.trim_matches(['[', ']']),
            port_out,
            path
        )
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(url: &str, action: Action) -> Rule {
        Rule {
            id: 1,
            name: "Test".into(),
            enabled: true,
            method: None,
            url: url.into(),
            app: None,
            action,
        }
    }

    fn rewrite(actions: Vec<RewriteAction>) -> Compiled {
        Compiled::new(rule("*", Action::Rewrite { actions })).unwrap()
    }

    #[test]
    fn wildcards_match_scheme_host_port_path_and_optional_query() {
        let matches = |pattern: &str, url: &str| Pattern::new(pattern).unwrap().matches(url);
        assert!(matches(
            "https://api.example.com/v1/*",
            "https://api.example.com/v1/users"
        ));
        assert!(matches(
            "HTTPS://API.Example.com/v1/*",
            "https://api.example.com/v1/"
        ));
        assert!(!matches(
            "https://api.example.com/v1/*",
            "http://api.example.com/v1/users"
        ));
        assert!(!matches(
            "https://api.example.com/V1/*",
            "https://api.example.com/v1/users"
        ));
        assert!(matches(
            "api.example.com",
            "http://api.example.com/anything?x=1"
        ));
        assert!(matches("*.example.com/*", "https://cdn.example.com/a.js"));
        assert!(!matches("*.example.com/*", "https://example.org/a.js"));
        assert!(matches(
            "http://localhost:8080/*",
            "http://localhost:8080/x"
        ));
        assert!(!matches(
            "http://localhost:8080/*",
            "http://localhost:8081/x"
        ));
        assert!(matches(
            "https://example.com:443/a",
            "https://example.com/a"
        ));
        // A pattern without a query ignores it; one with a query matches it.
        assert!(matches("http://x/search", "http://x/search?q=1"));
        assert!(matches("http://x/search?q=*", "http://x/search?q=1"));
        assert!(!matches("http://x/search?q=2", "http://x/search?q=1"));
        assert!(matches("*", "http://[::1]:9000/"));
        assert!(matches("http://x/*/b/*", "http://x/a/b/c"));
        assert!(!matches("http://x/*/b", "http://x/a/c"));
        assert!(Pattern::new("").is_err());
        assert!(Pattern::new("http://x/ y").is_err());
        assert!(Pattern::new("http:///x").is_err());
        assert_eq!(
            rule_url("http", "::1", 9000, "/a?b"),
            "http://[::1]:9000/a?b"
        );
        assert_eq!(rule_url("https", "x.com", 443, "*"), "https://x.com/");
        assert_eq!(
            pattern_from_url("https://Api.Example.com:443/v1/users?token=[redacted]"),
            "https://api.example.com/v1/users"
        );
        assert_eq!(
            pattern_from_url("http://localhost:8080/a"),
            "http://localhost:8080/a"
        );
    }

    #[test]
    fn rules_validate_and_normalize_every_field() {
        let valid = rule(
            " http://x/* ",
            Action::Breakpoint {
                request: true,
                response: false,
            },
        );
        let normalized = Rule {
            method: Some(" post ".into()),
            app: Some("  ".into()),
            ..valid.clone()
        }
        .normalize()
        .unwrap();
        assert_eq!(normalized.method.as_deref(), Some("POST"));
        assert_eq!(normalized.app, None);
        assert_eq!(normalized.url, "http://x/*");
        let invalid = |rule: Rule| assert!(rule.normalize().is_err());
        invalid(Rule {
            name: " ".into(),
            ..valid.clone()
        });
        invalid(Rule {
            name: "x".repeat(101),
            ..valid.clone()
        });
        invalid(Rule {
            method: Some("BAD METHOD".into()),
            ..valid.clone()
        });
        invalid(rule(
            "*",
            Action::Breakpoint {
                request: false,
                response: false,
            },
        ));
        invalid(rule(
            "*",
            Action::MapLocal {
                path: "relative.json".into(),
                status: 200,
                content_type: None,
                headers: Vec::new(),
            },
        ));
        invalid(rule(
            "*",
            Action::MapLocal {
                path: "/tmp/a.json".into(),
                status: 99,
                content_type: None,
                headers: Vec::new(),
            },
        ));
        invalid(rule(
            "*",
            Action::MapRemote {
                scheme: Some("ftp".into()),
                host: None,
                port: None,
                path: None,
                keep_query: true,
            },
        ));
        invalid(rule(
            "*",
            Action::MapRemote {
                scheme: None,
                host: None,
                port: None,
                path: None,
                keep_query: true,
            },
        ));
        invalid(rule(
            "*",
            Action::MapRemote {
                scheme: None,
                host: Some("example.com:80".into()),
                port: None,
                path: None,
                keep_query: true,
            },
        ));
        invalid(rule(
            "*",
            Action::Rewrite {
                actions: Vec::new(),
            },
        ));
        for action in [
            RewriteAction::SetHeader {
                side: RuleSide::Request,
                name: "content-length".into(),
                value: "1".into(),
            },
            RewriteAction::AddHeader {
                side: RuleSide::Request,
                name: "bad name".into(),
                value: "1".into(),
            },
            RewriteAction::AddHeader {
                side: RuleSide::Request,
                name: "x".into(),
                value: "a\nb".into(),
            },
            RewriteAction::Status { code: 101 },
            RewriteAction::ReplaceBody {
                side: RuleSide::Response,
                find: "(".into(),
                replace: String::new(),
                regex: true,
            },
            RewriteAction::ReplaceBody {
                side: RuleSide::Response,
                find: "a{1000}{1000}{1000}".into(),
                replace: String::new(),
                regex: true,
            },
            RewriteAction::RemoveQuery {
                name: String::new(),
            },
        ] {
            invalid(rule(
                "*",
                Action::Rewrite {
                    actions: vec![action],
                },
            ));
        }
        let remote = rule(
            "*",
            Action::MapRemote {
                scheme: Some("HTTPS".into()),
                host: Some("API.Example.COM.".into()),
                port: None,
                path: Some("/v2".into()),
                keep_query: false,
            },
        )
        .normalize()
        .unwrap();
        assert!(matches!(
            remote.action,
            Action::MapRemote { scheme: Some(ref s), host: Some(ref h), .. }
                if s == "https" && h == "api.example.com"
        ));
    }

    #[test]
    fn saved_rules_round_trip_and_are_capped() {
        let rules: Vec<_> = (0..250)
            .map(|id| Rule {
                id,
                ..rule(
                    "http://x/*",
                    Action::Breakpoint {
                        request: true,
                        response: true,
                    },
                )
            })
            .collect();
        let mut json = serde_json::to_value(&rules).unwrap();
        json.as_array_mut()
            .unwrap()
            .insert(0, serde_json::json!({"id": 0, "bad": true}));
        json.as_array_mut()
            .unwrap()
            .insert(1, serde_json::to_value(&rules[0]).unwrap());
        let loaded = load_rules(&json);
        assert_eq!(loaded.len(), RULE_LIMIT);
        assert_eq!(loaded[0], rules[0]);
        assert_eq!(loaded[1].id, 1);
        assert!(load_rules(&serde_json::json!("not a list")).is_empty());
        let local = rule(
            "*",
            Action::MapLocal {
                path: "/tmp/a.json".into(),
                status: 201,
                content_type: None,
                headers: vec![("x-mock".into(), "1".into())],
            },
        );
        let json = serde_json::to_value(&local).unwrap();
        assert_eq!(json["action"]["kind"], "mapLocal");
        assert_eq!(json["action"]["path"], "/tmp/a.json");
        assert_eq!(load_rules(&serde_json::json!([json])), vec![local]);
        assert!(RuleSet::new(rules, true).is_err());
    }

    #[test]
    fn plans_follow_order_and_the_breakpoint_switch() {
        let rules = vec![
            Rule {
                id: 1,
                ..rule(
                    "http://x/*",
                    Action::MapRemote {
                        scheme: None,
                        host: Some("y".into()),
                        port: None,
                        path: None,
                        keep_query: true,
                    },
                )
            },
            Rule {
                id: 2,
                ..rule(
                    "http://x/*",
                    Action::MapLocal {
                        path: "/tmp/x".into(),
                        status: 200,
                        content_type: None,
                        headers: Vec::new(),
                    },
                )
            },
            Rule {
                id: 3,
                method: Some("POST".into()),
                ..rule(
                    "*",
                    Action::Breakpoint {
                        request: true,
                        response: true,
                    },
                )
            },
            Rule {
                id: 4,
                app: Some("Curl".into()),
                ..rule(
                    "*",
                    Action::Rewrite {
                        actions: vec![RewriteAction::Status { code: 404 }],
                    },
                )
            },
        ];
        let set = RuleSet::new(rules.clone(), true).unwrap();
        let plan = set.plan("POST", "http://x/a", Some("curl"));
        assert_eq!(plan.mapping.unwrap().rule.id, 1);
        assert_eq!(plan.rewrites.len(), 1);
        assert_eq!(plan.request_break.unwrap().rule.id, 3);
        assert!(plan.response_break.is_some());
        let plan = set.plan("GET", "http://z/a", None);
        assert!(plan.is_empty());
        let off = RuleSet::new(rules, false).unwrap();
        assert!(off.plan("POST", "http://z/", None).is_empty());
    }

    #[test]
    fn header_status_and_query_actions() {
        let rule = rewrite(vec![
            RewriteAction::AddHeader {
                side: RuleSide::Request,
                name: "x-added".into(),
                value: "1".into(),
            },
            RewriteAction::SetHeader {
                side: RuleSide::Request,
                name: "accept".into(),
                value: "text/plain".into(),
            },
            RewriteAction::RemoveHeader {
                side: RuleSide::Request,
                name: "cookie".into(),
            },
            RewriteAction::SetHeader {
                side: RuleSide::Response,
                name: "x-response".into(),
                value: "only response".into(),
            },
            RewriteAction::Status { code: 418 },
            RewriteAction::SetQuery {
                name: "page".into(),
                value: "2 & 3".into(),
            },
            RewriteAction::RemoveQuery {
                name: "debug flag".into(),
            },
        ]);
        let mut headers = HeaderMap::new();
        headers.append("accept", "a".parse().unwrap());
        headers.append("accept", "b".parse().unwrap());
        headers.insert("cookie", "secret".parse().unwrap());
        assert_eq!(rewrite_headers(&rule, RuleSide::Request, &mut headers), 3);
        assert_eq!(headers.get_all("accept").iter().count(), 1);
        assert_eq!(headers["accept"], "text/plain");
        assert_eq!(headers["x-added"], "1");
        assert!(!headers.contains_key("cookie"));
        assert!(!headers.contains_key("x-response"));
        let mut status = StatusCode::OK;
        assert!(rewrite_status(&rule, &mut status));
        assert_eq!(status, StatusCode::IM_A_TEAPOT);
        let uri: Uri = "http://x/list?page=1&debug+flag=on&page=9&q=a"
            .parse()
            .unwrap();
        assert_eq!(
            rewrite_query(&rule, &uri).unwrap().to_string(),
            "http://x/list?page=2%20%26%203&q=a"
        );
        let uri: Uri = "/plain".parse().unwrap();
        assert_eq!(
            rewrite_query(&rule, &uri).unwrap().to_string(),
            "/plain?page=2%20%26%203"
        );
    }

    #[test]
    fn body_actions_rewrite_text_and_skip_what_they_cannot() {
        let rule = rewrite(vec![
            RewriteAction::ReplaceBody {
                side: RuleSide::Response,
                find: "\"plan\":\\s*\"(\\w+)\"".into(),
                replace: "\"plan\": \"pro-$1\"".into(),
                regex: true,
            },
            RewriteAction::ReplaceBody {
                side: RuleSide::Response,
                find: "Alice".into(),
                replace: "Bob".into(),
                regex: false,
            },
        ]);
        let mut headers = HeaderMap::new();
        assert_eq!(
            rewrite_body(
                &rule,
                RuleSide::Response,
                &mut headers,
                br#"{"user":"Alice","plan": "free"}"#
            ),
            BodyEdit::Changed(br#"{"user":"Bob","plan": "pro-free"}"#.to_vec())
        );
        assert_eq!(
            rewrite_body(&rule, RuleSide::Request, &mut headers, b"Alice"),
            BodyEdit::Unchanged
        );
        assert_eq!(
            rewrite_body(&rule, RuleSide::Response, &mut headers, b"nothing"),
            BodyEdit::Unchanged
        );
        assert_eq!(
            rewrite_body(&rule, RuleSide::Response, &mut headers, &[0xff, 0xfe]),
            BodyEdit::Skipped("Body skipped: not text")
        );
        assert_eq!(
            rewrite_body(
                &rule,
                RuleSide::Response,
                &mut headers,
                &vec![b'a'; EDIT_BODY_LIMIT + 1]
            ),
            BodyEdit::Skipped("Body skipped: over 1 MB")
        );

        // Compressed bodies are decoded, rewritten and sent uncompressed.
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gzip, b"hello Alice").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        assert_eq!(
            rewrite_body(
                &rule,
                RuleSide::Response,
                &mut headers,
                &gzip.finish().unwrap()
            ),
            BodyEdit::Changed(b"hello Bob".to_vec())
        );
        assert!(!headers.contains_key("content-encoding"));
        headers.insert("content-encoding", "zstd".parse().unwrap());
        assert!(matches!(
            rewrite_body(&rule, RuleSide::Response, &mut headers, b"x"),
            BodyEdit::Skipped(_)
        ));
        // A runaway expansion is refused rather than buffered.
        let grow = rewrite(vec![RewriteAction::ReplaceBody {
            side: RuleSide::Request,
            find: "a".into(),
            replace: "b".repeat(8 * 1024),
            regex: false,
        }]);
        assert_eq!(
            rewrite_body(
                &grow,
                RuleSide::Request,
                &mut HeaderMap::new(),
                &vec![b'a'; 1024]
            ),
            BodyEdit::Skipped("Body skipped: the result would exceed 4 MB")
        );
        let mut framed = HeaderMap::new();
        framed.insert("content-length", "5".parse().unwrap());
        framed.insert("transfer-encoding", "chunked".parse().unwrap());
        reframe(&mut framed);
        assert!(framed.is_empty());
    }

    #[test]
    fn map_local_reads_only_small_regular_files() {
        let dir = std::env::temp_dir().join(format!("zentop-map-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mock.json");
        std::fs::write(&file, br#"{"ok":true}"#).unwrap();
        let path = file.to_str().unwrap();
        assert_eq!(
            read_local(path, MAP_LOCAL_LIMIT).unwrap(),
            br#"{"ok":true}"#
        );
        assert_eq!(
            read_local(path, 4),
            Err("Map Local file is larger than 10 MB")
        );
        assert_eq!(
            read_local(dir.join("missing").to_str().unwrap(), MAP_LOCAL_LIMIT),
            Err("Map Local file not found")
        );
        assert_eq!(
            read_local(dir.to_str().unwrap(), MAP_LOCAL_LIMIT),
            Err("Map Local path is not a regular file")
        );
        #[cfg(unix)]
        {
            let fifo = dir.join("fifo");
            let status = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(
                read_local(fifo.to_str().unwrap(), MAP_LOCAL_LIMIT),
                Err("Map Local path is not a regular file")
            );
        }
        let local = rule(
            "*",
            Action::MapLocal {
                path: path.into(),
                status: 201,
                content_type: None,
                headers: vec![("x-mock".into(), "yes".into())],
            },
        );
        let head = local_head(&local, 11).unwrap();
        assert_eq!(head.status, StatusCode::CREATED);
        assert_eq!(head.headers["content-type"], "application/json");
        assert_eq!(head.headers["content-length"], "11");
        assert_eq!(head.headers["x-mock"], "yes");
        assert_eq!(content_type_for("/a/b.PNG"), "image/png");
        assert_eq!(content_type_for("/a/b"), "application/octet-stream");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn map_remote_builds_the_new_destination() {
        let remote = |scheme: Option<&str>, host: Option<&str>, port, path: Option<&str>, keep| {
            rule(
                "*",
                Action::MapRemote {
                    scheme: scheme.map(Into::into),
                    host: host.map(Into::into),
                    port,
                    path: path.map(Into::into),
                    keep_query: keep,
                },
            )
            .normalize()
            .unwrap()
        };
        let url = |rule: &Rule| remote_url(rule, "http", "api.example.com", 8080, "/v1/a?x=1");
        assert_eq!(
            url(&remote(None, None, Some(9000), None, true)).unwrap(),
            "http://api.example.com:9000/v1/a?x=1"
        );
        assert_eq!(
            url(&remote(Some("https"), None, None, None, true)).unwrap(),
            "https://api.example.com/v1/a?x=1"
        );
        assert_eq!(
            url(&remote(None, Some("localhost"), None, Some("/v2"), false)).unwrap(),
            "http://localhost/v2"
        );
        assert_eq!(
            url(&remote(None, None, None, Some("/v2"), true)).unwrap(),
            "http://api.example.com:8080/v2?x=1"
        );
        assert_eq!(
            url(&remote(None, Some("[::1]"), Some(81), None, true)).unwrap(),
            "http://[::1]:81/v1/a?x=1"
        );
    }
}
