//! Read-only disk usage scanning and frontend-independent treemap navigation.
mod layout;
mod scan;
pub use layout::{layout, Rect, Tile};
pub use scan::{scan, Progress, ScanJob, ScanOptions};
pub use upstream::treemap::Rect as PixelRect;

use std::{collections::HashSet, ffi::OsString, path::PathBuf, sync::Arc, time::SystemTime};

mod upstream;
pub use upstream::classify::{Category, Reclaim};
/// The full upstream MIT notice, embedded for frontend distribution.
pub const LICENSE: &str = include_str!("../../../../licenses/disktree-MIT.md");
pub use upstream::size::{human_bytes_short, human_count, share, share_bar};
impl Category {
    pub const ALL: [Self; 9] = [
        Self::Code,
        Self::AgentScratch,
        Self::Toolchain,
        Self::Synced,
        Self::Git,
        Self::Media,
        Self::Documents,
        Self::Cache,
        Self::Other,
    ];
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Self::Code => [112, 151, 224],
            Self::AgentScratch => [212, 151, 102],
            Self::Toolchain => [94, 196, 147],
            Self::Synced => [97, 187, 205],
            Self::Git => [210, 116, 151],
            Self::Media => [174, 125, 220],
            Self::Documents => [161, 170, 190],
            Self::Cache => [211, 188, 100],
            Self::Other => [123, 138, 167],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review {
    Build,
    Cache,
    Worktree,
    Other(Reclaim),
}
impl Review {
    pub fn hint(self) -> &'static str {
        match self {
            Self::Build => "Generated build output; review before removing",
            Self::Cache => "Cache; review before removing",
            Self::Worktree => "Worktree; may contain uncommitted work",
            Self::Other(reason) => reason.label(),
        }
    }
}
#[derive(Debug)]
pub struct Node {
    pub name: OsString,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub is_dir: bool,
    pub symlink: bool,
    pub hidden: bool,
    pub apparent: u64,
    pub allocated: u64,
    pub files: u64,
    pub dirs: u64,
    pub modified: Option<SystemTime>,
    pub category: Category,
    pub review: Option<Review>,
    pub issue: Option<String>,
    pub own_files: u64,
    pub own_apparent: u64,
    pub own_allocated: u64,
}
#[derive(Debug)]
pub struct Volume {
    pub mount: PathBuf,
    pub total: u64,
    pub available: u64,
}
#[derive(Debug)]
pub struct Tree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub volume: Option<Volume>,
    pub skipped: u64,
    pub errors: Vec<String>,
    pub truncated: bool,
    pub allocated_supported: bool,
    pub elapsed: std::time::Duration,
}
impl Tree {
    pub fn path(&self, mut id: usize) -> PathBuf {
        let mut parts = Vec::new();
        while let Some(parent) = self.nodes[id].parent {
            parts.push(&self.nodes[id].name);
            id = parent;
        }
        let mut path = self.root.clone();
        for part in parts.into_iter().rev() {
            path.push(part);
        }
        path
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Metric {
    #[default]
    Size,
    Files,
    Age,
}
impl Metric {
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "Size",
            Self::Files => "Files",
            Self::Age => "Age",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Size => Self::Files,
            Self::Files => Self::Age,
            Self::Age => Self::Size,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ViewOptions {
    pub hidden: bool,
    pub apparent: bool,
    pub depth: usize,
    pub metric: Metric,
    pub filter: String,
}
impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            hidden: true,
            apparent: false,
            depth: 4,
            metric: Metric::Size,
            filter: String::new(),
        }
    }
}

pub struct View {
    pub tree: Arc<Tree>,
    pub options: ViewOptions,
    pub zoom: usize,
    pub selected: usize,
    pub marked: HashSet<usize>,
    pub weights: Vec<f64>,
    pub bytes: Vec<u64>,
    pub files: Vec<u64>,
    pub dirs: Vec<u64>,
    review_candidates: Vec<usize>,
}
impl View {
    pub fn new(tree: Tree) -> Self {
        let review_candidates = tree
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.review.is_some())
            .map(|(i, _)| i)
            .collect();
        let mut view = Self {
            review_candidates,
            weights: vec![],
            bytes: vec![],
            files: vec![],
            dirs: vec![],
            tree: Arc::new(tree),
            options: ViewOptions::default(),
            zoom: 0,
            selected: 0,
            marked: HashSet::new(),
        };
        view.rebuild();
        view
    }
    pub fn rebuild(&mut self) {
        let n = self.tree.nodes.len();
        self.weights = vec![0.; n];
        self.bytes = vec![0; n];
        self.files = vec![0; n];
        self.dirs = vec![0; n];
        let mut visible = vec![true; n];
        let mut matches = vec![false; n];
        let query = self.options.filter.to_lowercase();
        for (i, node) in self.tree.nodes.iter().enumerate() {
            visible[i] = i == 0
                || (node.parent.is_some_and(|p| visible[p])
                    && (self.options.hidden || !node.hidden));
            matches[i] = query.is_empty()
                || node.name.to_string_lossy().to_lowercase().contains(&query)
                || node.parent.is_some_and(|p| matches[p]);
            if !visible[i] || !matches[i] {
                continue;
            }
            let bytes = if self.options.apparent || !self.tree.allocated_supported {
                node.own_apparent
            } else {
                node.own_allocated
            };
            self.bytes[i] = bytes;
            self.files[i] = node.own_files;
            self.dirs[i] = u64::from(node.is_dir && i != 0);
            self.weights[i] = match self.options.metric {
                Metric::Size => bytes as f64,
                Metric::Files => node.own_files as f64,
                Metric::Age => bytes as f64,
            };
        }
        for i in (1..n).rev() {
            if !visible[i] {
                continue;
            }
            if !matches[i] && self.weights[i] > 0. && self.tree.nodes[i].is_dir {
                self.dirs[i] = self.dirs[i].saturating_add(1);
            }
            if let Some(parent) = self.tree.nodes[i].parent {
                self.weights[parent] += self.weights[i];
                self.bytes[parent] = self.bytes[parent].saturating_add(self.bytes[i]);
                self.files[parent] = self.files[parent].saturating_add(self.files[i]);
                self.dirs[parent] = self.dirs[parent].saturating_add(self.dirs[i]);
            }
        }
        if self.weights[self.zoom] <= 0. {
            self.zoom = 0;
            self.selected = 0;
        }
        if self.weights[self.selected] <= 0. {
            self.selected = self.zoom;
        }
    }
    pub fn tiles(&self, width: f64, height: f64, header: f64, gap: f64) -> Vec<Tile> {
        layout(self, width, height, header, gap)
    }
    pub fn enter(&mut self) {
        if self.tree.nodes[self.selected].is_dir && self.weights[self.selected] > 0. {
            self.zoom = self.selected;
        }
    }
    pub fn up(&mut self) {
        if let Some(parent) = self.tree.nodes[self.zoom].parent {
            self.selected = self.zoom;
            self.zoom = parent;
        }
    }
    pub fn reset(&mut self) {
        self.zoom = 0;
        self.selected = 0;
    }
    pub fn mark(&mut self) {
        let mut ancestor = Some(self.selected);
        while let Some(id) = ancestor {
            if self.marked.remove(&id) {
                return;
            }
            ancestor = self.tree.nodes[id].parent;
        }
        let selected = self.selected;
        self.marked.retain(|id| {
            let mut parent = self.tree.nodes[*id].parent;
            while let Some(p) = parent {
                if p == selected {
                    return false;
                }
                parent = self.tree.nodes[p].parent;
            }
            true
        });
        self.marked.insert(selected);
    }
    pub fn marked_bytes(&self) -> u64 {
        self.marked.iter().fold(0_u64, |total, id| {
            total.saturating_add(if self.options.apparent || !self.tree.allocated_supported {
                self.tree.nodes[*id].apparent
            } else {
                self.tree.nodes[*id].allocated
            })
        })
    }
    pub fn color(&self, id: usize) -> [u8; 3] {
        if self.options.metric != Metric::Age {
            return self.tree.nodes[id].category.rgb();
        }
        match self.tree.nodes[id]
            .modified
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map(|d| d.as_secs() / 86400)
        {
            Some(0..=7) => [94, 196, 147],
            Some(8..=30) => [97, 187, 205],
            Some(31..=365) => [174, 125, 220],
            Some(_) => [212, 151, 102],
            None => [123, 138, 167],
        }
    }
    pub fn hatched(&self, id: usize) -> bool {
        let mut ancestor = Some(id);
        while let Some(i) = ancestor {
            if matches!(
                self.tree.nodes[i].review,
                Some(Review::Build | Review::Cache | Review::Other(_))
            ) {
                return true;
            }
            ancestor = self.tree.nodes[i].parent;
        }
        false
    }
    pub fn review(&self) -> Vec<usize> {
        let mut candidates: Vec<_> = self
            .review_candidates
            .iter()
            .map(|i| (*i, &self.tree.nodes[*i]))
            .filter(|(i, n)| {
                if n.review.is_none() || self.bytes[*i] == 0 {
                    return false;
                }
                let mut parent = n.parent;
                while let Some(p) = parent {
                    if p == self.zoom {
                        return true;
                    }
                    if self.tree.nodes[p].review.is_some() {
                        return false;
                    }
                    parent = self.tree.nodes[p].parent;
                }
                *i == self.zoom
            })
            .map(|(i, _)| i)
            .collect();
        candidates.sort_by_key(|i| std::cmp::Reverse(self.bytes[*i]));
        candidates.truncate(12);
        candidates
    }
    /// Spatial arrow navigation between the currently displayed rectangles.
    pub fn move_selection(&mut self, tiles: &[Tile], dx: f64, dy: f64) {
        let current = tiles
            .iter()
            .find(|t| t.id == self.selected)
            .map(|t| t.rect)
            .unwrap_or(Rect {
                x: 0.,
                y: 0.,
                w: 0.,
                h: 0.,
            });
        let (cx, cy) = (current.x + current.w / 2., current.y + current.h / 2.);
        let next = tiles
            .iter()
            .filter(|t| t.id != self.selected)
            .filter_map(|t| {
                let x = t.rect.x + t.rect.w / 2. - cx;
                let y = t.rect.y + t.rect.h / 2. - cy;
                let forward = x * dx + y * dy;
                if forward <= 0. {
                    return None;
                }
                let sideways = (x * dy - y * dx).abs();
                Some((forward + sideways * 3., t.id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        if let Some(id) = next {
            self.selected = id;
        }
    }
}

pub fn classify(name: &str, parent: Category) -> (Category, Option<Review>) {
    let kind = upstream::classify::category_of_name(name).unwrap_or(parent);
    (
        kind,
        if name.eq_ignore_ascii_case("worktrees") {
            Some(Review::Worktree)
        } else {
            None
        },
    )
}
pub fn review_of(
    name: &str,
    parent: Category,
    has_sibling: impl Fn(&str) -> bool,
) -> Option<Review> {
    upstream::classify::reclaim_of(name, parent, has_sibling)
        .map(|reason| match reason {
            Reclaim::BuildOutput => Review::Build,
            Reclaim::Regenerable => Review::Cache,
            other => Review::Other(other),
        })
        .or_else(|| {
            name.eq_ignore_ascii_case("worktrees")
                .then_some(Review::Worktree)
        })
}

#[cfg(test)]
mod tests;

/// UI-owned scan lifecycle. Dropping/replacing a job requests cancellation.
#[derive(Default)]
pub struct Session {
    pub view: Option<View>,
    pub job: Option<ScanJob>,
    pub error: Option<String>,
    pub path: Option<PathBuf>,
}
impl Session {
    pub fn start(&mut self, path: PathBuf) {
        self.job = None;
        self.error = None;
        self.path = Some(path.clone());
        match ScanJob::start(path) {
            Ok(job) => self.job = Some(job),
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub fn poll(&mut self) -> bool {
        let Some(result) = self.job.as_ref().and_then(ScanJob::poll) else {
            return false;
        };
        self.job = None;
        match result {
            Ok(tree) => {
                let options = self.view.as_ref().map(|v| v.options.clone());
                let mut view = View::new(tree);
                if let Some(options) = options {
                    view.options = options;
                    view.rebuild();
                }
                self.view = Some(view);
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
    pub fn cancel(&mut self) {
        if self.job.take().is_some() {
            self.error = Some("Scan cancelled".into());
        }
    }
}
pub fn format_size(bytes: u64) -> String {
    upstream::size::human_bytes(bytes)
}
pub fn age(time: Option<SystemTime>) -> String {
    let Some(seconds) = time
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs())
    else {
        return "Unknown".into();
    };
    match seconds {
        0..=59 => "Just now".into(),
        60..=3599 => format!("{} minutes ago", seconds / 60),
        3600..=86399 => format!("{} hours ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86400),
    }
}
