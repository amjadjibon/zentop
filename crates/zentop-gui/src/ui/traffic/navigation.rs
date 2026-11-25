use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::{identity, Identity};
use zentop_proxy::Metadata;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    App(AppKey),
    Domain(String),
    Host(String),
}

/// Apps group by name, as in Proxyman: every `curl` run is one "curl" entry,
/// not one entry per process. Attributed and unknown flows never share one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AppKey {
    pub(super) name: String,
    pub(super) known: bool,
}

impl AppKey {
    fn from_row(row: &Metadata) -> Self {
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
}

fn hostname(authority: &str) -> &str {
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
}

fn domain(host: &str) -> &str {
    if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    psl::domain_str(host).unwrap_or(host)
}

impl Scope {
    pub(super) fn matches(&self, row: &Metadata) -> bool {
        match self {
            Self::App(app) => {
                app.name == row.process.name() && app.known == identity(&row.process).is_some()
            }
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
    pub(super) fn update(&mut self, before: Option<&Metadata>, after: Option<&Metadata>) {
        let app = |row: &Metadata| AppKey::from_row(row);
        let prior = before.map(app);
        let next = after.map(app);
        if prior != next {
            if let Some(key) = prior {
                if let Some(group) = self.apps.get_mut(&key) {
                    group.count -= 1;
                    if group.count == 0 {
                        self.apps.remove(&key);
                    }
                }
            }
            if let Some(key) = next {
                self.apps.entry(key).or_default().count += 1;
            }
        }
        if let Some(row) = after {
            if let Some(id) = identity(&row.process) {
                if let Some(group) = self.apps.get_mut(&AppKey::from_row(row)) {
                    group.identity = Some(id);
                }
            }
        }

        let prior = before.map(|row| hostname(&row.host));
        let next = after.map(|row| hostname(&row.host));
        if prior == next {
            return;
        }
        if let Some(host) = prior {
            let name = domain(host);
            if let Some(group) = self.domains.get_mut(name) {
                decrement(&mut group.hosts, host);
                group.count -= 1;
                if group.count == 0 {
                    self.domains.remove(name);
                }
            }
        }
        if let Some(host) = next {
            let group = self.domains.entry(domain(host).into()).or_default();
            group.count += 1;
            *group.hosts.entry(host.into()).or_default() += 1;
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Column {
    Time,
    Process,
    Method,
    Host,
    Path,
    Status,
    Size,
    Duration,
}

impl Column {
    pub(super) const ALL: [Self; 8] = [
        Self::Time,
        Self::Process,
        Self::Method,
        Self::Host,
        Self::Path,
        Self::Status,
        Self::Size,
        Self::Duration,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Time => "Time",
            Self::Process => "Process",
            Self::Method => "Method",
            Self::Host => "Host",
            Self::Path => "Path",
            Self::Status => "Status",
            Self::Size => "Size",
            Self::Duration => "Duration",
        }
    }

    pub(super) fn width(self) -> f32 {
        match self {
            Self::Time => 62.,
            Self::Process => 130.,
            Self::Method => 62.,
            Self::Host => 138.,
            Self::Path => 0.,
            Self::Status => 50.,
            Self::Size => 62.,
            Self::Duration => 72.,
        }
    }

    fn compare(self, a: &Metadata, b: &Metadata) -> Ordering {
        match self {
            Self::Time => a.started.cmp(&b.started),
            Self::Process => a.process.name().cmp(b.process.name()),
            Self::Method => a.method.cmp(&b.method),
            Self::Host => a.host.cmp(&b.host),
            Self::Path => a.path.cmp(&b.path),
            Self::Status => a.status.cmp(&b.status),
            Self::Size => (a.bytes.0 + a.bytes.1).cmp(&(b.bytes.0 + b.bytes.1)),
            Self::Duration => a.timing.total.cmp(&b.timing.total),
        }
    }
}

pub(super) struct Sort {
    pub(super) column: Column,
    pub(super) descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            column: Column::Time,
            descending: true,
        }
    }
}

impl Sort {
    pub(super) fn select(&mut self, column: Column) {
        self.descending = if self.column == column {
            !self.descending
        } else {
            false
        };
        self.column = column;
    }

    pub(super) fn order(&self, a: &Metadata, b: &Metadata) -> Ordering {
        let order = self.column.compare(a, b);
        let order = if self.descending {
            order.reverse()
        } else {
            order
        };
        order.then_with(|| b.id.cmp(&a.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zentop_proxy::{Attribution, FlowKind, FlowState};

    fn row(id: u64, host: &str) -> Metadata {
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
        }
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
        let mut known = a.clone();
        known.process = Attribution::Known {
            unique_id: 1,
            pid: 2,
            start_time: 3,
            name: "curl".into(),
        };
        index.update(Some(&a), Some(&known));
        assert_eq!(index.apps[&unknown].count, 2);
        assert!(Scope::App(AppKey::from_row(&known)).matches(&known));
        // A second run of the same program is the same app, not a new entry.
        let mut rerun = b.clone();
        rerun.process = Attribution::Known {
            unique_id: 9,
            pid: 8,
            start_time: 7,
            name: "curl".into(),
        };
        index.update(Some(&b), Some(&rerun));
        let curl = AppKey::from_row(&known);
        assert_eq!(index.apps[&curl].count, 2);
        assert_eq!(index.apps[&curl].identity, Some((8, 7)));
        assert!(Scope::App(curl.clone()).matches(&rerun));
        assert!(!Scope::App(curl).matches(&c));
        index.update(Some(&rerun), Some(&b));
        index.update(Some(&known), None);
        assert_eq!(index.apps.len(), 1);
        assert_eq!(index.domains["example.co.uk"].count, 1);
    }

    #[test]
    fn sorts_by_value_with_stable_id_ties() {
        let a = row(1, "z:80");
        let mut b = row(2, "a:80");
        let mut sort = Sort::default();
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
        sort.select(Column::Host);
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
        sort.select(Column::Host);
        assert_eq!(sort.order(&a, &b), Ordering::Less);
        sort.select(Column::Size);
        b.bytes.0 = 20;
        assert_eq!(sort.order(&a, &b), Ordering::Less);
        sort.select(Column::Status);
        b.status = Some(5);
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
    }
}
