//! Which rows the table shows: content-type tabs, the filter bar, the
//! sidebar scope and a process opened from its details sheet.
use super::navigation::Scope;
use super::{identity, Identity};
use zentop_proxy::{ContentKind, FlowKind, Metadata};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Transport {
    #[default]
    All,
    Http,
    Https,
    WebSocket,
}

impl Transport {
    pub(super) const ALL: [Self; 4] = [Self::All, Self::Http, Self::Https, Self::WebSocket];

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Http => "HTTP",
            Self::Https => "HTTPS",
            Self::WebSocket => "WebSocket",
        }
    }
}

/// Encrypted toward the client: decrypted HTTPS, blind tunnels, and upgrades
/// inside decrypted connections.
pub(super) fn secure(row: &Metadata) -> bool {
    matches!(row.kind, FlowKind::Tunnel | FlowKind::DecryptedHttps)
        || row.url.starts_with("https://")
        || row.url.starts_with("wss://")
}

/// The tab strip: one transport and, optionally, one content kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Tabs {
    pub(super) transport: Transport,
    pub(super) content: Option<ContentKind>,
}

impl Tabs {
    pub(super) fn matches(&self, row: &Metadata) -> bool {
        let transport = match self.transport {
            Transport::All => true,
            Transport::Http => !secure(row),
            Transport::Https => secure(row),
            Transport::WebSocket => row.websocket,
        };
        transport && self.content.is_none_or(|content| row.content == content)
    }

    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A snapshot of every active filter, built once per pass over the rows.
#[derive(Default)]
pub(super) struct RowFilter {
    pub(super) search: String,
    pub(super) process: String,
    pub(super) host: String,
    pub(super) method: String,
    pub(super) status: Option<u16>,
    pub(super) errors: bool,
    pub(super) tabs: Tabs,
    pub(super) scope: Option<Scope>,
    pub(super) process_scope: Option<Identity>,
}

impl RowFilter {
    pub(super) fn matches(&self, row: &Metadata) -> bool {
        let lower = |text: &str| text.to_lowercase();
        let process = lower(row.process.name());
        let host = lower(&row.host);
        let method = lower(&row.method);
        self.tabs.matches(row)
            && self
                .process_scope
                .is_none_or(|id| identity(&row.process) == Some(id))
            && self.scope.as_ref().is_none_or(|scope| scope.matches(row))
            && process.contains(&self.process)
            && host.contains(&self.host)
            && method.contains(&self.method)
            && self
                .status
                .is_none_or(|class| row.status.is_some_and(|code| code / 100 == class))
            && (!self.errors || row.state.has_error() || row.status.is_some_and(|n| n >= 400))
            && (self.search.is_empty()
                || process.contains(&self.search)
                || host.contains(&self.search)
                || method.contains(&self.search)
                || lower(&row.url).contains(&self.search))
    }
}

#[cfg(test)]
mod tests {
    use super::super::navigation::tests::row;
    use super::*;

    #[test]
    fn tabs_split_transports_and_content_kinds() {
        let plain = row(1, "a.test:80");
        let mut decrypted = row(2, "b.test:443");
        decrypted.kind = FlowKind::DecryptedHttps;
        decrypted.url = "https://b.test/".into();
        decrypted.content = ContentKind::Json;
        let mut tunnel = row(3, "c.test:443");
        tunnel.kind = FlowKind::Tunnel;
        tunnel.url = "c.test:443".into();
        let mut socket = row(4, "d.test:80");
        socket.kind = FlowKind::Upgrade;
        socket.websocket = true;
        let mut secure_socket = socket.clone();
        secure_socket.url = "https://d.test/ws".into();
        let rows = [&plain, &decrypted, &tunnel, &socket, &secure_socket];
        let ids = |tabs: Tabs| {
            rows.iter()
                .filter(|row| tabs.matches(row))
                .map(|row| row.id)
                .collect::<Vec<_>>()
        };
        let tab = |transport, content| Tabs { transport, content };
        assert_eq!(ids(Tabs::default()), [1, 2, 3, 4, 4]);
        assert_eq!(ids(tab(Transport::Http, None)), [1, 4]);
        assert_eq!(ids(tab(Transport::Https, None)), [2, 3, 4]);
        assert_eq!(ids(tab(Transport::WebSocket, None)), [4, 4]);
        assert_eq!(ids(tab(Transport::All, Some(ContentKind::Json))), [2]);
        // Tunnels sit under both HTTPS and Other.
        assert_eq!(ids(tab(Transport::Https, Some(ContentKind::Other))), [3, 4]);
        assert_eq!(
            ids(tab(Transport::Http, Some(ContentKind::Json))),
            Vec::<u64>::new()
        );
        assert!(Tabs::default().is_default());
    }

    #[test]
    fn filter_bar_fields_combine() {
        let mut a = row(1, "api.example.com:443");
        a.url = "https://api.example.com/v1/users?page=2".into();
        a.status = Some(404);
        let filter = |build: fn(&mut RowFilter)| {
            let mut filter = RowFilter::default();
            build(&mut filter);
            filter.matches(&a)
        };
        assert!(filter(|_| {}));
        assert!(filter(|f| f.search = "users".into()));
        assert!(!filter(|f| f.search = "orders".into()));
        assert!(filter(|f| f.status = Some(4)));
        assert!(!filter(|f| f.status = Some(2)));
        assert!(filter(|f| f.errors = true));
        assert!(filter(|f| f.method = "ge".into()));
        assert!(!filter(|f| f.host = "other".into()));
        assert!(!filter(|f| f.process_scope = Some((1, 2))));
    }
}
