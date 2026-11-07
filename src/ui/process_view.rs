//! Searching, filtering, sorting and grouping processes for the table.
//! Pure functions so the rules can be tested without a window.
use crate::monitoring::ProcessInfo;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

pub type Identity = (u32, u64);

pub fn identity(process: &ProcessInfo) -> Identity {
    (process.pid, process.start_time)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Pid,
    Status,
    User,
    Cpu,
    Gpu,
    Memory,
    Virtual,
    Disk,
    Ppid,
    Root,
    Command,
    Environ,
    Session,
    StartTime,
    RunTime,
}

impl Column {
    pub const ALL: [Column; 16] = [
        Column::Name,
        Column::Pid,
        Column::Status,
        Column::User,
        Column::Cpu,
        Column::Gpu,
        Column::Memory,
        Column::Virtual,
        Column::Disk,
        Column::Ppid,
        Column::Root,
        Column::Command,
        Column::Environ,
        Column::Session,
        Column::StartTime,
        Column::RunTime,
    ];

    /// The preference key, shared with earlier releases' settings files.
    pub fn key(self) -> &'static str {
        match self {
            Column::Name => "name",
            Column::Pid => "pid",
            Column::Status => "status",
            Column::User => "user",
            Column::Cpu => "cpu_usage",
            Column::Gpu => "gpu_usage",
            Column::Memory => "memory_usage",
            Column::Virtual => "virtual_memory",
            Column::Disk => "disk_usage",
            Column::Ppid => "ppid",
            Column::Root => "root",
            Column::Command => "command",
            Column::Environ => "environ",
            Column::Session => "session_id",
            Column::StartTime => "start_time",
            Column::RunTime => "run_time",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Column::Name => "Process Name",
            Column::Pid => "PID",
            Column::Status => "Status",
            Column::User => "User",
            Column::Cpu => "CPU %",
            Column::Gpu => "GPU %",
            Column::Memory => "RAM",
            Column::Virtual => "VIRT",
            Column::Disk => "Disk I/O (R/W)",
            Column::Ppid => "Parent PID",
            Column::Root => "Root",
            Column::Command => "Command",
            Column::Environ => "Environment Variables",
            Column::Session => "Session ID",
            Column::StartTime => "Start Time",
            Column::RunTime => "Run Time",
        }
    }

    pub fn numeric(self) -> bool {
        matches!(
            self,
            Column::Cpu
                | Column::Gpu
                | Column::Memory
                | Column::Virtual
                | Column::Disk
                | Column::RunTime
        )
    }

    /// Default widths: the standard columns fit the 1120pt minimum window.
    pub fn width(self) -> f32 {
        match self {
            Column::Name => 240.,
            Column::Command | Column::Environ | Column::Root => 300.,
            Column::Disk => 120.,
            Column::StartTime => 160.,
            Column::RunTime => 110.,
            Column::Status => 100.,
            Column::User | Column::Memory | Column::Virtual => 90.,
            _ => 80.,
        }
    }

    /// The cell text, also used for tooltips and accessibility labels.
    pub fn text(self, p: &ProcessInfo) -> String {
        match self {
            Column::Name => p.name.clone(),
            Column::Pid => p.pid.to_string(),
            Column::Status => p.status.clone(),
            Column::User => p.user.clone(),
            Column::Cpu => format!("{:.1}%", p.cpu_usage),
            Column::Gpu => p
                .gpu_usage
                .map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
            Column::Memory => format_mb(p.memory_usage),
            Column::Virtual => format!("{:.1} GB", p.virtual_memory as f64 / GIB),
            Column::Disk => format!(
                "{:.1}/{:.1} MB",
                p.disk_usage.0 as f64 / MIB,
                p.disk_usage.1 as f64 / MIB
            ),
            Column::Ppid => p.ppid.to_string(),
            Column::Root => p.root.clone(),
            Column::Command => p.command.clone(),
            Column::Environ => p.environ.join(" "),
            Column::Session => p.session_id.map_or_else(|| "—".into(), |v| v.to_string()),
            Column::StartTime => format_timestamp(p.start_time),
            Column::RunTime => {
                let s = p.run_time;
                format!("{}h {}m {}s", s / 3600, (s % 3600) / 60, s % 60)
            }
        }
    }
}

pub const MIB: f64 = 1024.0 * 1024.0;
pub const GIB: f64 = MIB * 1024.0;

pub fn format_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / MIB)
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn format_uptime(seconds: u64) -> String {
    format!(
        "{}d {}h {}m",
        seconds / 86400,
        (seconds % 86400) / 3600,
        (seconds % 3600) / 60
    )
}

pub fn format_timestamp(seconds: u64) -> String {
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Gt,
    Lt,
    Eq,
    Ge,
    Le,
}

impl Operator {
    pub const ALL: [Self; 5] = [Self::Gt, Self::Lt, Self::Eq, Self::Ge, Self::Le];
    pub fn symbol(self) -> &'static str {
        match self {
            Operator::Gt => ">",
            Operator::Lt => "<",
            Operator::Eq => "=",
            Operator::Ge => "≥",
            Operator::Le => "≤",
        }
    }
    fn test(self, value: f64, target: f64) -> bool {
        match self {
            Operator::Gt => value > target,
            Operator::Lt => value < target,
            Operator::Eq => value == target,
            Operator::Ge => value >= target,
            Operator::Le => value <= target,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub enabled: bool,
    pub operator: Operator,
    pub value: f64,
}

impl Rule {
    const fn new(value: f64) -> Self {
        Self {
            enabled: false,
            operator: Operator::Gt,
            value,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    Cpu,
    Gpu,
    Memory,
    Runtime,
}

impl RuleKind {
    pub const ALL: [RuleKind; 4] = [
        RuleKind::Cpu,
        RuleKind::Gpu,
        RuleKind::Memory,
        RuleKind::Runtime,
    ];
    pub fn label(self) -> &'static str {
        match self {
            RuleKind::Cpu => "CPU usage",
            RuleKind::Gpu => "GPU engine activity",
            RuleKind::Memory => "Memory",
            RuleKind::Runtime => "Run time",
        }
    }
    pub fn unit(self) -> &'static str {
        match self {
            RuleKind::Cpu | RuleKind::Gpu => "%",
            RuleKind::Memory => "MB",
            RuleKind::Runtime => "min",
        }
    }
}

pub const STATUSES: [&str; 5] = ["Running", "Sleeping", "Idle", "Stopped", "Zombie"];

#[derive(Clone, Debug, PartialEq)]
pub struct Filters {
    pub cpu: Rule,
    pub gpu: Rule,
    pub memory: Rule,
    pub runtime: Rule,
    pub statuses: Vec<String>,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            cpu: Rule::new(50.0),
            gpu: Rule::new(1.0),
            memory: Rule::new(100.0),
            runtime: Rule::new(60.0),
            statuses: Vec::new(),
        }
    }
}

impl Filters {
    pub fn rule(&self, kind: RuleKind) -> &Rule {
        match kind {
            RuleKind::Cpu => &self.cpu,
            RuleKind::Gpu => &self.gpu,
            RuleKind::Memory => &self.memory,
            RuleKind::Runtime => &self.runtime,
        }
    }
    pub fn rule_mut(&mut self, kind: RuleKind) -> &mut Rule {
        match kind {
            RuleKind::Cpu => &mut self.cpu,
            RuleKind::Gpu => &mut self.gpu,
            RuleKind::Memory => &mut self.memory,
            RuleKind::Runtime => &mut self.runtime,
        }
    }
    pub fn active_count(&self) -> usize {
        RuleKind::ALL
            .iter()
            .filter(|k| self.rule(**k).enabled)
            .count()
            + usize::from(!self.statuses.is_empty())
    }
}

/// `:3000` or `port:443`. Bare digits stay PID/name searches.
pub fn parse_port_term(term: &str) -> Option<u16> {
    let term = term.trim();
    let lower = term.to_ascii_lowercase();
    let digits = lower
        .strip_prefix("port:")
        .or_else(|| lower.strip_prefix(':'))?;
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u32 = digits.parse().ok()?;
    (1..=65535).contains(&port).then_some(port as u16)
}

pub fn has_port_query(search: &str) -> bool {
    !search.is_empty() && search.split(',').any(|t| parse_port_term(t).is_some())
}

/// Compiled comma-separated search terms.
pub struct Query {
    terms: Vec<Term>,
}

enum Term {
    Port(u16),
    Text {
        raw: String,
        lower: String,
        regex: Option<regex::Regex>,
    },
}

impl Query {
    pub fn new(search: &str) -> Self {
        let terms = if search.is_empty() {
            Vec::new()
        } else {
            search
                .split(',')
                .map(str::trim)
                .map(|term| match parse_port_term(term) {
                    Some(port) => Term::Port(port),
                    None => Term::Text {
                        raw: term.to_owned(),
                        lower: term.to_lowercase(),
                        regex: regex::RegexBuilder::new(term)
                            .case_insensitive(true)
                            .size_limit(1 << 16)
                            .build()
                            .ok(),
                    },
                })
                .collect()
        };
        Self { terms }
    }

    fn matches(&self, p: &ProcessInfo) -> bool {
        if self.terms.is_empty() {
            return true;
        }
        let name = p.name.to_lowercase();
        let command = p.command.to_lowercase();
        let pid = p.pid.to_string();
        self.terms.iter().any(|term| match term {
            Term::Port(port) => p.ports.contains(port),
            Term::Text { raw, lower, regex } => {
                name.contains(lower.as_str())
                    || command.contains(lower.as_str())
                    || pid.contains(raw.as_str())
                    || regex.as_ref().is_some_and(|r| r.is_match(&p.name))
            }
        })
    }
}

pub fn matches(p: &ProcessInfo, query: &Query, filters: &Filters) -> bool {
    if !filters.statuses.is_empty() && !filters.statuses.contains(&p.status) {
        return false;
    }
    let rule = |r: &Rule, value: f64| !r.enabled || r.operator.test(value, r.value);
    if !rule(&filters.cpu, p.cpu_usage as f64) {
        return false;
    }
    // Missing GPU counters never pass as a measured zero.
    if filters.gpu.enabled
        && !p
            .gpu_usage
            .filter(|v| v.is_finite())
            .is_some_and(|v| filters.gpu.operator.test(v as f64, filters.gpu.value))
    {
        return false;
    }
    if !rule(&filters.memory, p.memory_usage as f64 / MIB) {
        return false;
    }
    if !rule(&filters.runtime, p.run_time as f64 / 60.0) {
        return false;
    }
    query.matches(p)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub ascending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            column: Column::Cpu,
            ascending: false,
        }
    }
}

fn compare_disk(a: (u64, u64), b: (u64, u64)) -> Ordering {
    let (reads, writes) = ((a.0 + b.0) as f64, (a.1 + b.1) as f64);
    if writes > reads * 1.5 {
        a.1.cmp(&b.1).then(a.0.cmp(&b.0))
    } else if reads > writes * 1.5 {
        a.0.cmp(&b.0).then(a.1.cmp(&b.1))
    } else {
        (a.0 + a.1)
            .cmp(&(b.0 + b.1))
            .then(a.0.max(a.1).cmp(&b.0.max(b.1)))
    }
}

fn compare(column: Column, a: &ProcessInfo, b: &ProcessInfo) -> Ordering {
    match column {
        Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        Column::Pid => a.pid.cmp(&b.pid),
        Column::Status => a.status.cmp(&b.status),
        Column::User => a.user.cmp(&b.user),
        Column::Cpu => a.cpu_usage.total_cmp(&b.cpu_usage),
        Column::Gpu => a
            .gpu_usage
            .unwrap_or(f32::NEG_INFINITY)
            .total_cmp(&b.gpu_usage.unwrap_or(f32::NEG_INFINITY)),
        Column::Memory => a.memory_usage.cmp(&b.memory_usage),
        Column::Virtual => a.virtual_memory.cmp(&b.virtual_memory),
        Column::Disk => compare_disk(a.disk_usage, b.disk_usage),
        Column::Ppid => a.ppid.cmp(&b.ppid),
        Column::Root => a.root.cmp(&b.root),
        Column::Command => a.command.cmp(&b.command),
        Column::Environ => a.environ.len().cmp(&b.environ.len()),
        Column::Session => a.session_id.cmp(&b.session_id),
        Column::StartTime => a.start_time.cmp(&b.start_time),
        Column::RunTime => a.run_time.cmp(&b.run_time),
    }
}

/// Pinned commands first, then the chosen column; stable for equal keys.
pub fn sort(
    indices: &mut [usize],
    processes: &[ProcessInfo],
    sort: Sort,
    pinned: &HashSet<String>,
) {
    indices.sort_by(|&a, &b| {
        let (a, b) = (&processes[a], &processes[b]);
        let pin = pinned
            .contains(&b.command)
            .cmp(&pinned.contains(&a.command));
        pin.then_with(|| {
            let order = compare(sort.column, a, b);
            if sort.ascending {
                order
            } else {
                order.reverse()
            }
        })
    });
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub index: usize,
    /// Position of this row's family among the roots, for paging by family.
    pub root_index: usize,
    pub depth: usize,
    pub child_count: usize,
    pub expanded: bool,
    pub family_cpu: f64,
    pub family_memory: u64,
    pub family_count: usize,
}

#[derive(Default)]
struct Total {
    cpu: f64,
    memory: u64,
    count: usize,
    pinned: bool,
}

/// Each process's place in a family, as indexes into `processes`. A helper
/// that macOS attributes to an app (see `ProcessInfo::owner`) belongs to that
/// app; everything else belongs to its parent. A parent that started after
/// its child is a reused PID and is ignored, and cycles are cut.
pub fn family_parents(processes: &[ProcessInfo], order: &[usize]) -> HashMap<usize, usize> {
    let by_pid: HashMap<u32, usize> = order.iter().map(|&i| (processes[i].pid, i)).collect();
    let mut parents: HashMap<usize, usize> = HashMap::new();
    for &i in order {
        let p = &processes[i];
        let candidates = p.owner.into_iter().chain([p.ppid]);
        let parent = candidates
            .filter_map(|pid| by_pid.get(&pid).copied())
            .find(|&parent| {
                let q = &processes[parent];
                let newer_parent =
                    q.start_time > 0 && p.start_time > 0 && q.start_time > p.start_time;
                parent != i && !newer_parent
            });
        if let Some(parent) = parent {
            parents.insert(i, parent);
        }
    }
    // Resolve every parent chain once; cut an edge where a cycle closes.
    let mut resolved = HashSet::new();
    for &start in order {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut current = start;
        while !resolved.contains(&current) {
            if !seen.insert(current) {
                parents.remove(&current);
                break;
            }
            chain.push(current);
            match parents.get(&current) {
                Some(&parent) => current = parent,
                None => break,
            }
        }
        resolved.extend(chain);
    }
    parents
}

/// Groups processes into families. `matching` limits rows to matches and their
/// ancestors and keeps those branches open; aggregates always cover the whole
/// family. Memory is summed RSS, so shared pages can be counted more than once.
pub fn build_tree(
    processes: &[ProcessInfo],
    order: &[usize],
    expanded: &HashSet<Identity>,
    matching: Option<&HashSet<usize>>,
    sort: Sort,
    pinned: &HashSet<String>,
) -> (Vec<TreeRow>, usize) {
    let parents = family_parents(processes, order);

    let mut all_children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut all_roots = Vec::new();
    for &i in order {
        match parents.get(&i) {
            Some(&parent) => all_children.entry(parent).or_default().push(i),
            None => all_roots.push(i),
        }
    }

    let mut walk = Vec::with_capacity(order.len());
    let mut stack = all_roots.clone();
    while let Some(i) = stack.pop() {
        walk.push(i);
        stack.extend(all_children.get(&i).into_iter().flatten().copied());
    }
    let mut totals: HashMap<usize, Total> = HashMap::new();
    for &i in walk.iter().rev() {
        let p = &processes[i];
        let mut total = Total {
            cpu: if p.cpu_usage.is_finite() {
                p.cpu_usage.max(0.0) as f64
            } else {
                0.0
            },
            memory: p.memory_usage,
            count: 1,
            pinned: pinned.contains(&p.command),
        };
        for child in all_children.get(&i).into_iter().flatten() {
            if let Some(c) = totals.get(child) {
                total.cpu += c.cpu;
                total.memory += c.memory;
                total.count += c.count;
                total.pinned |= c.pinned;
            }
        }
        totals.insert(i, total);
    }

    let mut included = HashSet::new();
    match matching {
        None => included.extend(order.iter().copied()),
        Some(matching) => {
            for &i in matching {
                let mut current = i;
                while included.insert(current) {
                    match parents.get(&current) {
                        Some(&parent) => current = parent,
                        None => break,
                    }
                }
            }
        }
    }

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for &i in order {
        if !included.contains(&i) {
            continue;
        }
        match parents.get(&i).filter(|p| included.contains(p)) {
            Some(&parent) => children.entry(parent).or_default().push(i),
            None => roots.push(i),
        }
    }
    let family_order = |a: &usize, b: &usize| {
        let (a, b) = (&totals[a], &totals[b]);
        b.pinned.cmp(&a.pinned).then_with(|| {
            let order = match sort.column {
                Column::Cpu => a.cpu.total_cmp(&b.cpu),
                Column::Memory => a.memory.cmp(&b.memory),
                _ => Ordering::Equal,
            };
            if sort.ascending {
                order
            } else {
                order.reverse()
            }
        })
    };
    roots.sort_by(family_order);
    for siblings in children.values_mut() {
        siblings.sort_by(family_order);
    }

    let mut rows = Vec::new();
    for (root_index, &root) in roots.iter().enumerate() {
        let mut stack = vec![(root, 0)];
        while let Some((i, depth)) = stack.pop() {
            let descendants = children.get(&i).map(Vec::as_slice).unwrap_or_default();
            let total = &totals[&i];
            let open = !descendants.is_empty()
                && (matching.is_some() || expanded.contains(&identity(&processes[i])));
            rows.push(TreeRow {
                index: i,
                root_index,
                depth,
                child_count: descendants.len(),
                expanded: open,
                family_cpu: total.cpu,
                family_memory: total.memory,
                family_count: total.count,
            });
            if open {
                stack.extend(descendants.iter().rev().map(|&c| (c, depth + 1)));
            }
        }
    }
    (rows, roots.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, ppid: u32, name: &str) -> ProcessInfo {
        ProcessInfo {
            gpu_usage: None,
            pid,
            ppid,
            owner: None,
            name: name.into(),
            cpu_usage: 0.0,
            memory_usage: 0,
            status: "Running".into(),
            user: "me".into(),
            command: format!("/bin/{name}"),
            threads: None,
            environ: vec![],
            root: "/".into(),
            virtual_memory: 0,
            start_time: pid as u64,
            run_time: 0,
            disk_usage: (0, 0),
            session_id: None,
            ports: vec![],
        }
    }

    #[test]
    fn port_syntax_validates_ranges_and_keeps_bare_numbers_as_pids() {
        assert_eq!(parse_port_term(":3000"), Some(3000));
        assert_eq!(parse_port_term(" PORT:443 "), Some(443));
        assert_eq!(parse_port_term("3000"), None);
        assert_eq!(parse_port_term(":0"), None);
        assert_eq!(parse_port_term(":65536"), None);
        assert_eq!(parse_port_term(":123456"), None);
        assert!(has_port_query("node, :8080"));
        assert!(!has_port_query("node, 8080"));
        let mut p = process(8080, 1, "web");
        p.ports = vec![3000];
        let f = Filters::default();
        assert!(matches(&p, &Query::new(":3000"), &f));
        assert!(!matches(&p, &Query::new(":8080"), &f));
        assert!(matches(&p, &Query::new("8080"), &f));
    }

    #[test]
    fn gpu_filters_distinguish_unknown_from_zero() {
        let filters = Filters {
            gpu: Rule {
                enabled: true,
                operator: Operator::Ge,
                value: 0.0,
            },
            ..Filters::default()
        };
        let mut p = process(1, 0, "a");
        let q = Query::new("");
        assert!(!matches(&p, &q, &filters));
        p.gpu_usage = Some(0.0);
        assert!(matches(&p, &q, &filters));
        p.gpu_usage = Some(f32::NAN);
        assert!(!matches(&p, &q, &filters));
    }

    #[test]
    fn search_matches_name_command_pid_and_regex() {
        let f = Filters::default();
        let p = process(4242, 1, "Firefox");
        assert!(matches(&p, &Query::new("fire"), &f));
        assert!(matches(&p, &Query::new("bin/fire"), &f));
        assert!(matches(&p, &Query::new("424"), &f));
        assert!(matches(&p, &Query::new("^fire.*x$"), &f));
        assert!(matches(&p, &Query::new("nope, fox"), &f));
        assert!(!matches(&p, &Query::new("chrome"), &f));
        assert!(!matches(&p, &Query::new("[unclosed"), &f));
    }

    #[test]
    fn pinned_first_then_column() {
        let mut ps = vec![process(1, 0, "a"), process(2, 0, "b"), process(3, 0, "c")];
        ps[0].cpu_usage = 5.0;
        ps[1].cpu_usage = 50.0;
        ps[2].cpu_usage = 1.0;
        let pinned = HashSet::from([ps[2].command.clone()]);
        let mut order = vec![0, 1, 2];
        sort(&mut order, &ps, Sort::default(), &pinned);
        assert_eq!(order, vec![2, 1, 0]);
        sort(
            &mut order,
            &ps,
            Sort {
                column: Column::Name,
                ascending: true,
            },
            &HashSet::new(),
        );
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn helpers_join_the_app_macos_holds_responsible() {
        // launchd adopts every helper; the owner says which app each works for.
        let mut ps = vec![
            process(1, 0, "launchd"),
            process(100, 1, "NeoHtop"),
            process(101, 1, "WebContent"),
            process(102, 1, "GPU"),
            process(103, 1, "Networking"),
            process(104, 1, "orphaned helper"),
        ];
        for (i, cpu, memory) in [(1, 3.0, 90), (2, 2.0, 200), (3, 0.5, 40)] {
            ps[i].cpu_usage = cpu;
            ps[i].memory_usage = memory;
        }
        ps[2].owner = Some(100);
        ps[3].owner = Some(100);
        // An owner that started after the helper is a reused PID.
        ps[4].owner = Some(100);
        ps[4].start_time = 50;
        // An owner missing from the snapshot falls back to the real parent.
        ps[5].owner = Some(999);
        let order: Vec<usize> = (0..ps.len()).collect();
        let parents = family_parents(&ps, &order);
        assert_eq!(parents.get(&2), Some(&1));
        assert_eq!(parents.get(&3), Some(&1));
        assert_eq!(parents.get(&4), Some(&0));
        assert_eq!(parents.get(&5), Some(&0));

        let expanded = HashSet::from([identity(&ps[0])]);
        let (rows, _) = build_tree(
            &ps,
            &order,
            &expanded,
            None,
            Sort::default(),
            &HashSet::new(),
        );
        let app = rows.iter().find(|r| r.index == 1).unwrap();
        assert_eq!(app.depth, 1);
        assert_eq!(app.child_count, 2);
        assert_eq!(app.family_count, 3);
        assert!((app.family_cpu - 5.5).abs() < 1e-6);
        assert_eq!(app.family_memory, 330);
    }

    #[test]
    fn tree_aggregates_families_and_survives_cycles_and_reused_pids() {
        let mut ps = vec![
            process(1, 0, "init"),
            process(10, 1, "shell"),
            process(11, 10, "vim"),
            process(20, 21, "loop-a"),
            process(21, 20, "loop-b"),
        ];
        ps[1].cpu_usage = 2.0;
        ps[2].cpu_usage = 3.0;
        ps[2].memory_usage = 7;
        // A parent that started after its child is a reused PID, not the parent.
        let mut orphan = process(30, 11, "orphan");
        orphan.start_time = 5;
        ps.push(orphan);
        let order: Vec<usize> = (0..ps.len()).collect();
        let (rows, roots) = build_tree(
            &ps,
            &order,
            &HashSet::new(),
            None,
            Sort::default(),
            &HashSet::new(),
        );
        // init, the cycle (cut once), and the orphan with a newer "parent".
        assert_eq!(roots, 3);
        let init = rows.iter().find(|r| r.index == 0).unwrap();
        assert_eq!(init.family_count, 3);
        assert!((init.family_cpu - 5.0).abs() < 1e-6);
        assert_eq!(init.family_memory, 7);
        assert!(!init.expanded);
        assert_eq!(rows.len(), 3);

        let expanded = HashSet::from([identity(&ps[0])]);
        let (rows, _) = build_tree(
            &ps,
            &order,
            &expanded,
            None,
            Sort::default(),
            &HashSet::new(),
        );
        assert!(rows.iter().any(|r| r.index == 1 && r.depth == 1));
        assert!(!rows.iter().any(|r| r.index == 2));

        // Searches reveal matching descendants with their ancestors.
        let matching = HashSet::from([2]);
        let (rows, roots) = build_tree(
            &ps,
            &order,
            &HashSet::new(),
            Some(&matching),
            Sort::default(),
            &HashSet::new(),
        );
        assert_eq!(roots, 1);
        assert_eq!(
            rows.iter().map(|r| r.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(rows[0].family_count, 3);
    }

    #[test]
    fn disk_sort_follows_dominant_operation() {
        assert_eq!(compare_disk((0, 100), (0, 50)), Ordering::Greater);
        assert_eq!(compare_disk((100, 0), (50, 0)), Ordering::Greater);
        assert_eq!(compare_disk((10, 10), (5, 5)), Ordering::Greater);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_uptime(90061), "1d 1h 1m");
        let mut p = process(1, 0, "a");
        p.run_time = 3725;
        assert_eq!(Column::RunTime.text(&p), "1h 2m 5s");
        assert_eq!(Column::Gpu.text(&p), "—");
    }
}
