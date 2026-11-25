//! The sidebar's live index (apps, domains and hosts), pinned favorites, and
//! the flattened tree the sidebar renders.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::{identity, Identity};
use crate::preferences::{pin_name, PIN_LIMIT};
use serde_json::Value;
use zentop_proxy::Metadata;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Scope {
    App(AppKey),
    /// One registrable domain contacted by one app.
    AppDomain(AppKey, String),
    Domain(String),
    Host(String),
}

/// Apps group by name: every `curl` run is one "curl" entry, not one entry
/// per process. Attributed and unknown flows never share one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct AppKey {
    pub(super) name: String,
    pub(super) known: bool,
}

impl AppKey {
    pub(super) fn from_row(row: &Metadata) -> Self {
        Self {
            name: row.process.name().to_owned(),
            known: identity(&row.process).is_some(),
        }
    }
}

#[derive(Default)]
pub(super) struct AppGroup {
    pub(super) count: usize,
    /// The most recent process seen for this app, used to resolve its icon.
    pub(super) identity: Option<Identity>,
    /// Registrable domains this app contacted, with flow counts.
    pub(super) domains: BTreeMap<String, usize>,
}

pub(super) fn hostname(authority: &str) -> &str {
    if let Some(end) = authority.find(']').filter(|_| authority.starts_with('[')) {
        return &authority[..=end];
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
}

pub(super) fn domain(host: &str) -> &str {
    if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    psl::domain_str(host).unwrap_or(host)
}

impl Scope {
    pub(super) fn matches(&self, row: &Metadata) -> bool {
        let app = |app: &AppKey| {
            app.name == row.process.name() && app.known == identity(&row.process).is_some()
        };
        match self {
            Self::App(key) => app(key),
            Self::AppDomain(key, group) => app(key) && domain(hostname(&row.host)) == group,
            Self::Domain(group) => domain(hostname(&row.host)) == group,
            Self::Host(host) => hostname(&row.host) == host,
        }
    }
}

#[derive(Default)]
pub(super) struct Sidebar {
    pub(super) apps: BTreeMap<AppKey, AppGroup>,
    pub(super) domains: BTreeMap<String, DomainGroup>,
}

#[derive(Default)]
pub(super) struct DomainGroup {
    pub(super) count: usize,
    pub(super) hosts: BTreeMap<String, usize>,
}

impl Sidebar {
    /// Applies one row change incrementally: `before` leaves, `after` arrives.
    pub(super) fn update(&mut self, before: Option<&Metadata>, after: Option<&Metadata>) {
        let key = |row: &Metadata| (AppKey::from_row(row), hostname(&row.host).to_owned());
        let prior = before.map(key);
        let next = after.map(key);
        if prior != next {
            if let Some((app, host)) = prior {
                self.remove(&app, &host);
            }
            if let Some((app, host)) = next {
                self.add(app, &host);
            }
        }
        if let Some(row) = after {
            if let Some(id) = identity(&row.process) {
                if let Some(group) = self.apps.get_mut(&AppKey::from_row(row)) {
                    group.identity = Some(id);
                }
            }
        }
    }

    fn add(&mut self, app: AppKey, host: &str) {
        let name = domain(host);
        let group = self.apps.entry(app).or_default();
        group.count += 1;
        *group.domains.entry(name.to_owned()).or_default() += 1;
        let group = self.domains.entry(name.to_owned()).or_default();
        group.count += 1;
        *group.hosts.entry(host.to_owned()).or_default() += 1;
    }

    fn remove(&mut self, app: &AppKey, host: &str) {
        let name = domain(host);
        if let Some(group) = self.apps.get_mut(app) {
            decrement(&mut group.domains, name);
            group.count -= 1;
            if group.count == 0 {
                self.apps.remove(app);
            }
        }
        if let Some(group) = self.domains.get_mut(name) {
            decrement(&mut group.hosts, host);
            group.count -= 1;
            if group.count == 0 {
                self.domains.remove(name);
            }
        }
    }
}

fn decrement<K: Ord + std::borrow::Borrow<Q>, Q: Ord + ?Sized>(
    counts: &mut BTreeMap<K, usize>,
    key: &Q,
) {
    if let Some(value) = counts.get_mut(key) {
        *value -= 1;
        if *value == 0 {
            counts.remove(key);
        }
    }
}

/// Pinned apps and domains. Saved in the GUI settings with no traffic data,
/// just names, as the decrypt list is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Pins {
    pub(super) apps: BTreeSet<String>,
    pub(super) domains: BTreeSet<String>,
}

impl Pins {
    pub(super) fn load(prefs: &crate::preferences::Preferences) -> Self {
        let read = |key, domain| {
            prefs
                .behavior(key)
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|name| pin_name(name, domain))
                .collect()
        };
        Self {
            apps: read("trafficPinnedApps", false),
            domains: read("trafficPinnedDomains", true),
        }
    }

    pub(super) fn patch(&self) -> Value {
        serde_json::json!({"behavior": {
            "trafficPinnedApps": self.apps,
            "trafficPinnedDomains": self.domains,
        }})
    }

    pub(super) fn is_pinned(&self, scope: &Scope) -> bool {
        match scope {
            Scope::App(app) => app.known && self.apps.contains(&app.name),
            Scope::Domain(domain) => self.domains.contains(domain),
            _ => false,
        }
    }

    /// Pins or unpins an app (by name) or a registrable domain.
    pub(super) fn toggle(&mut self, scope: &Scope) -> bool {
        let (set, name) = match scope {
            Scope::App(app) if app.known => (&mut self.apps, pin_name(&app.name, false)),
            Scope::Domain(domain) => (&mut self.domains, pin_name(domain, true)),
            _ => return false,
        };
        let Some(name) = name else { return false };
        if !set.remove(&name) {
            if set.len() >= PIN_LIMIT {
                return false;
            }
            set.insert(name);
        }
        true
    }
}

/// Collapsible sidebar nodes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Node {
    Favorites,
    Pinned,
    All,
    Apps,
    Domains,
    App(AppKey),
    Domain(String),
}

pub(super) fn default_expanded() -> HashSet<Node> {
    HashSet::from([Node::Favorites, Node::Pinned, Node::All])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowIcon {
    None,
    App,
    Domain,
    Host,
    Pin,
}

/// One rendered sidebar row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TreeRow {
    pub(super) depth: u8,
    pub(super) label: String,
    pub(super) count: Option<usize>,
    /// Set when the row collapses; the bool is whether it's expanded.
    pub(super) node: Option<(Node, bool)>,
    pub(super) scope: Option<Scope>,
    pub(super) icon: RowIcon,
    /// The process whose icon represents an app row.
    pub(super) identity: Option<Identity>,
    pub(super) section: bool,
}

impl TreeRow {
    fn section(depth: u8, label: &str, node: Option<(Node, bool)>, count: Option<usize>) -> Self {
        Self {
            depth,
            label: label.into(),
            count,
            node,
            scope: None,
            icon: RowIcon::None,
            identity: None,
            section: true,
        }
    }

    fn item(depth: u8, label: &str, count: usize, scope: Scope, icon: RowIcon) -> Self {
        Self {
            depth,
            label: label.into(),
            count: Some(count),
            node: None,
            scope: Some(scope),
            icon,
            identity: None,
            section: false,
        }
    }

    #[cfg(test)]
    pub(super) fn expanded(&self) -> bool {
        self.node.as_ref().is_some_and(|(_, open)| *open)
    }
}

/// Flattens the sidebar into rows. A filter narrows every list and expands
/// the groups that hold a match.
pub(super) fn tree(
    index: &Sidebar,
    pins: &Pins,
    expanded: &HashSet<Node>,
    filter: &str,
) -> Vec<TreeRow> {
    let filter = filter.trim().to_lowercase();
    let filtering = !filter.is_empty();
    let hit = |text: &str| !filtering || text.to_lowercase().contains(&filter);
    let open = |node: &Node| filtering || expanded.contains(node);
    let collapsible = |node: Node| {
        let open = open(&node);
        Some((node, open))
    };
    let mut rows = Vec::new();

    rows.push(TreeRow::section(
        0,
        "Favorites",
        collapsible(Node::Favorites),
        None,
    ));
    if open(&Node::Favorites) {
        let pinned = pins.apps.len() + pins.domains.len();
        rows.push(TreeRow::section(
            1,
            "Pinned",
            collapsible(Node::Pinned),
            Some(pinned),
        ));
        if open(&Node::Pinned) {
            for name in pins.apps.iter().filter(|name| hit(name)) {
                let key = AppKey {
                    name: name.clone(),
                    known: true,
                };
                let group = index.apps.get(&key);
                rows.push(TreeRow {
                    identity: group.and_then(|group| group.identity),
                    ..TreeRow::item(
                        2,
                        name,
                        group.map_or(0, |group| group.count),
                        Scope::App(key),
                        RowIcon::App,
                    )
                });
            }
            for name in pins.domains.iter().filter(|name| hit(name)) {
                let count = index.domains.get(name).map_or(0, |group| group.count);
                rows.push(TreeRow::item(
                    2,
                    name,
                    count,
                    Scope::Domain(name.clone()),
                    RowIcon::Pin,
                ));
            }
        }
        // Reserved for phase 3B rules.
        rows.push(TreeRow::section(1, "Saved", None, None));
    }

    rows.push(TreeRow::section(0, "All", collapsible(Node::All), None));
    if !open(&Node::All) {
        return rows;
    }
    rows.push(TreeRow::section(
        1,
        "Apps",
        collapsible(Node::Apps),
        Some(index.apps.len()),
    ));
    if open(&Node::Apps) {
        for (app, group) in &index.apps {
            let app_hit = hit(&app.name);
            if !app_hit && !group.domains.keys().any(|name| hit(name)) {
                continue;
            }
            let node = Node::App(app.clone());
            // A filter that matches only domains opens the app to show them.
            let expand = expanded.contains(&node) || !app_hit;
            rows.push(TreeRow {
                identity: group.identity,
                node: Some((node, expand)),
                ..TreeRow::item(
                    2,
                    &app.name,
                    group.count,
                    Scope::App(app.clone()),
                    RowIcon::App,
                )
            });
            if expand {
                for (name, &count) in &group.domains {
                    if app_hit || hit(name) {
                        rows.push(TreeRow::item(
                            3,
                            name,
                            count,
                            Scope::AppDomain(app.clone(), name.clone()),
                            RowIcon::Domain,
                        ));
                    }
                }
            }
        }
    }
    rows.push(TreeRow::section(
        1,
        "Domains",
        collapsible(Node::Domains),
        Some(index.domains.len()),
    ));
    if open(&Node::Domains) {
        for (name, group) in &index.domains {
            let domain_hit = hit(name);
            if !domain_hit && !group.hosts.keys().any(|host| hit(host)) {
                continue;
            }
            let node = Node::Domain(name.clone());
            let expand = expanded.contains(&node) || !domain_hit;
            rows.push(TreeRow {
                node: Some((node, expand)),
                ..TreeRow::item(
                    2,
                    name,
                    group.count,
                    Scope::Domain(name.clone()),
                    RowIcon::Domain,
                )
            });
            if expand {
                for (host, &count) in &group.hosts {
                    if domain_hit || hit(host) {
                        rows.push(TreeRow::item(
                            3,
                            host,
                            count,
                            Scope::Host(host.clone()),
                            RowIcon::Host,
                        ));
                    }
                }
            }
        }
    }
    rows
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use zentop_proxy::{Attribution, FlowKind, FlowState};

    pub(in super::super) fn row(id: u64, host: &str) -> Metadata {
        Metadata {
            id,
            host: host.into(),
            connection: 0,
            started: std::time::SystemTime::UNIX_EPOCH,
            process: Attribution::Unknown("test"),
            kind: FlowKind::Http,
            repeated: false,
            method: "GET".into(),
            path: "/".into(),
            status: Some(200),
            timing: Default::default(),
            bytes: (1, 2),
            state: FlowState::Complete,
            url: format!("http://{host}/"),
            content: Default::default(),
            websocket: false,
        }
    }

    pub(in super::super) fn known(mut row: Metadata, name: &str, pid: u32) -> Metadata {
        row.process = Attribution::Known {
            unique_id: pid.into(),
            pid,
            start_time: 7,
            name: name.into(),
        };
        row
    }

    #[test]
    fn changes_counts_and_matches_public_suffix_groups() {
        let mut index = Sidebar::default();
        let a = row(1, "a.example.co.uk:443");
        let b = row(2, "b.example.co.uk:80");
        let c = row(3, "example.net:80");
        for row in [&a, &b, &c] {
            index.update(None, Some(row));
        }
        assert_eq!(index.domains["example.co.uk"].count, 2);
        assert_eq!(index.domains["example.co.uk"].hosts.len(), 2);
        let group = Scope::Domain("example.co.uk".into());
        assert!(group.matches(&b));
        assert!(!group.matches(&c));
        assert!(Scope::Host("a.example.co.uk".into()).matches(&a));
        let unknown = AppKey::from_row(&a);
        assert_eq!(index.apps[&unknown].count, 3);
        assert_eq!(index.apps[&unknown].domains["example.co.uk"], 2);
        let curl_a = known(a.clone(), "curl", 2);
        index.update(Some(&a), Some(&curl_a));
        assert_eq!(index.apps[&unknown].count, 2);
        assert_eq!(index.apps[&unknown].domains["example.co.uk"], 1);
        // A second run of the same program is the same app, not a new entry.
        let curl_b = known(b.clone(), "curl", 8);
        index.update(Some(&b), Some(&curl_b));
        let curl = AppKey::from_row(&curl_a);
        assert_eq!(index.apps[&curl].count, 2);
        assert_eq!(index.apps[&curl].domains["example.co.uk"], 2);
        assert_eq!(index.apps[&curl].identity, Some((8, 7)));
        assert!(Scope::App(curl.clone()).matches(&curl_b));
        assert!(Scope::AppDomain(curl.clone(), "example.co.uk".into()).matches(&curl_b));
        assert!(!Scope::AppDomain(curl.clone(), "example.net".into()).matches(&curl_b));
        assert!(!Scope::App(curl).matches(&c));
        index.update(Some(&curl_b), Some(&b));
        index.update(Some(&curl_a), None);
        assert_eq!(index.apps.len(), 1);
        assert_eq!(index.domains["example.co.uk"].count, 1);
        assert_eq!(hostname("[::1]:8080"), "[::1]");
        assert_eq!(domain("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn pins_round_trip_through_settings_without_traffic_data() {
        let mut pins = Pins::default();
        let curl = AppKey {
            name: "curl".into(),
            known: true,
        };
        assert!(pins.toggle(&Scope::App(curl.clone())));
        assert!(pins.toggle(&Scope::Domain("Example.COM".into())));
        // Unknown processes and single hosts aren't pinnable.
        let unknown = AppKey {
            name: "Unknown process".into(),
            known: false,
        };
        assert!(!pins.toggle(&Scope::App(unknown)));
        assert!(!pins.toggle(&Scope::Host("a.example.com".into())));
        assert!(pins.is_pinned(&Scope::App(curl.clone())));
        assert!(pins.is_pinned(&Scope::Domain("example.com".into())));

        let patch = pins.patch();
        assert_eq!(
            patch,
            serde_json::json!({"behavior": {
                "trafficPinnedApps": ["curl"], "trafficPinnedDomains": ["example.com"]
            }})
        );
        let dir = std::env::temp_dir().join(format!("zentop-pins-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut prefs = crate::preferences::Preferences::at(path.clone());
        prefs.update(patch).unwrap();
        // Saved settings hold only the names, and reload into the same pins.
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["behavior"]["trafficPinnedApps"],
            serde_json::json!(["curl"])
        );
        assert_eq!(Pins::load(&prefs), pins);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(pins.toggle(&Scope::App(curl.clone())));
        assert!(!pins.is_pinned(&Scope::App(curl)));
    }

    #[test]
    fn tree_expands_groups_and_filters_matches() {
        let mut index = Sidebar::default();
        for (id, host, app) in [
            (1, "api.example.com:443", "curl"),
            (2, "cdn.example.com:443", "curl"),
            (3, "other.net:80", "node"),
        ] {
            index.update(None, Some(&known(row(id, host), app, id as u32)));
        }
        let mut pins = Pins::default();
        pins.toggle(&Scope::Domain("example.com".into()));
        let labels = |rows: &[TreeRow]| {
            rows.iter()
                .map(|row| format!("{}{}", " ".repeat(row.depth.into()), row.label))
                .collect::<Vec<_>>()
        };

        let mut expanded = default_expanded();
        let rows = tree(&index, &pins, &expanded, "");
        assert_eq!(
            labels(&rows),
            [
                "Favorites",
                " Pinned",
                "  example.com",
                " Saved",
                "All",
                " Apps",
                " Domains"
            ]
        );
        assert_eq!(rows[2].count, Some(2));
        assert_eq!(rows[5].count, Some(2));
        assert!(!rows[5].expanded());

        expanded.insert(Node::Apps);
        expanded.insert(Node::App(AppKey {
            name: "curl".into(),
            known: true,
        }));
        let rows = tree(&index, &pins, &expanded, "");
        assert_eq!(
            labels(&rows)[5..],
            [" Apps", "  curl", "   example.com", "  node", " Domains"]
        );
        assert_eq!(rows[7].count, Some(2));
        assert!(matches!(rows[7].scope, Some(Scope::AppDomain(..))));
        assert!(rows[6].expanded() && !rows[8].expanded());

        // Filtering opens every group with a match and hides the rest.
        let rows = tree(&index, &pins, &default_expanded(), "cdn");
        assert_eq!(
            labels(&rows),
            [
                "Favorites",
                " Pinned",
                " Saved",
                "All",
                " Apps",
                " Domains",
                "  example.com",
                "   cdn.example.com",
            ]
        );
        let rows = tree(&index, &pins, &default_expanded(), "NODE");
        assert_eq!(labels(&rows)[4..], [" Apps", "  node", " Domains"]);
    }
}
