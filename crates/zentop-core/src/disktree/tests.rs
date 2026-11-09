use super::*;
use std::{
    fs,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zentop-disktree-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("src/target")).unwrap();
        fs::create_dir_all(path.join(".cache")).unwrap();
        fs::write(path.join("src/main.rs"), vec![1; 2048]).unwrap();
        fs::write(path.join("src/target/output"), vec![1; 4096]).unwrap();
        fs::write(path.join(".cache/item"), vec![1; 1024]).unwrap();
        fs::write(path.join("empty"), []).unwrap();
        fs::write(path.join("src/Cargo.toml"), []).unwrap();
        Self(path)
    }
    fn scan(&self) -> Tree {
        scan(
            &self.0,
            &ScanOptions::default(),
            &AtomicBool::new(false),
            &Progress::default(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn sizes_counts_paths_and_categories() {
    let fixture = Fixture::new();
    let tree = fixture.scan();
    let root = &tree.nodes[0];
    assert_eq!(root.apparent, 7168);
    assert_eq!(root.files, 5);
    assert_eq!(root.dirs, 3);
    let (id, node) = tree
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.name == "main.rs")
        .unwrap();
    assert_eq!(node.category, Category::Code);
    assert_eq!(
        tree.path(id),
        fs::canonicalize(&fixture.0).unwrap().join("src/main.rs")
    );
    assert_eq!(
        tree.nodes
            .iter()
            .find(|n| n.name == "target")
            .unwrap()
            .review,
        Some(Review::Build)
    );
}
#[test]
fn hidden_filter_and_review_are_consistent() {
    let fixture = Fixture::new();
    let mut view = View::new(fixture.scan());
    view.options.apparent = true;
    view.options.hidden = false;
    view.rebuild();
    assert_eq!(view.bytes[0], 6144);
    assert_eq!(view.files[0], 4);
    assert_eq!(view.review().len(), 1);
    view.options.filter = "main.rs".into();
    view.rebuild();
    assert_eq!(view.bytes[0], 2048);
    assert_eq!(view.files[0], 1);
    assert_eq!(view.dirs[0], 1);
}

#[test]
fn rebuild_reuses_buffers_without_retaining_filtered_totals() {
    let fixture = Fixture::new();
    let mut view = View::new(fixture.scan());
    let buffers = (
        view.weights.as_ptr(),
        view.bytes.as_ptr(),
        view.files.as_ptr(),
        view.dirs.as_ptr(),
    );
    view.options.apparent = true;
    for _ in 0..3 {
        view.options.filter = "nothing matches".into();
        view.rebuild();
        assert_eq!(
            (view.weights[0], view.bytes[0], view.files[0], view.dirs[0]),
            (0., 0, 0, 0)
        );
        view.options.filter = "main.rs".into();
        view.options.metric = Metric::Files;
        view.rebuild();
        assert_eq!(
            (view.weights[0], view.bytes[0], view.files[0], view.dirs[0]),
            (1., 2048, 1, 1)
        );
    }
    assert_eq!(
        buffers,
        (
            view.weights.as_ptr(),
            view.bytes.as_ptr(),
            view.files.as_ptr(),
            view.dirs.as_ptr()
        )
    );
}

#[test]
fn replacement_releases_snapshot_preserves_settings_and_resets_node_ids() {
    let fixture = Fixture::new();
    let mut view = View::new(fixture.scan());
    let previous = Arc::downgrade(&view.tree);
    let buffers = (
        view.weights.as_ptr(),
        view.bytes.as_ptr(),
        view.files.as_ptr(),
        view.dirs.as_ptr(),
    );
    view.options = ViewOptions {
        hidden: false,
        apparent: true,
        depth: 2,
        metric: Metric::Files,
        filter: "main.rs".into(),
    };
    view.rebuild();
    view.selected = view
        .tree
        .nodes
        .iter()
        .position(|n| n.name == "src")
        .unwrap();
    view.enter();
    view.mark();
    fs::remove_dir_all(fixture.0.join("src/target")).unwrap();
    fs::write(fixture.0.join("src/main.rs"), [1; 17]).unwrap();
    view.replace_tree(fixture.scan());
    assert!(previous.upgrade().is_none());
    assert_eq!(
        buffers,
        (
            view.weights.as_ptr(),
            view.bytes.as_ptr(),
            view.files.as_ptr(),
            view.dirs.as_ptr()
        )
    );
    assert_eq!((view.zoom, view.selected), (0, 0));
    assert!(view.marked.is_empty());
    assert!(!view.options.hidden && view.options.apparent);
    assert_eq!(
        (
            view.options.depth,
            view.options.metric,
            view.options.filter.as_str()
        ),
        (2, Metric::Files, "main.rs")
    );
    assert_eq!((view.weights[0], view.bytes[0], view.files[0]), (1., 17, 1));
    assert!(view.review().is_empty());
}

#[test]
fn smaller_replacement_releases_oversized_buffers_and_review_candidates() {
    let large = Fixture::new();
    for i in 0..128 {
        fs::create_dir_all(large.0.join(format!("branch-{i}/.cache"))).unwrap();
    }
    let mut view = View::new(large.scan());
    assert!(view.review_candidates.len() > 64);
    let small = Fixture::new();
    view.replace_tree(small.scan());
    let limit = (view.tree.nodes.len() * 2).max(64);
    assert!([
        view.weights.capacity(),
        view.bytes.capacity(),
        view.files.capacity(),
        view.dirs.capacity()
    ]
    .iter()
    .all(|c| *c <= limit));
    assert!(view.review_candidates.capacity() <= 64);
    assert_eq!(view.review().len(), 2);
    assert_eq!(view.files[0], 5);
}

#[test]
fn capacity_hints_preserve_counts_and_obey_entry_limits() {
    let fixture = Fixture::new();
    for hint in [0, 1, 9, 32] {
        let tree = scan(
            &fixture.0,
            &ScanOptions {
                initial_capacity: hint,
                ..Default::default()
            },
            &AtomicBool::new(false),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(
            (
                tree.nodes[0].apparent,
                tree.nodes[0].files,
                tree.nodes[0].dirs
            ),
            (7168, 5, 3)
        );
        assert!(tree.nodes.capacity() <= tree.nodes.len() + tree.nodes.len() / 8);
        assert!(tree
            .nodes
            .iter()
            .all(|n| n.children.capacity() == n.children.len()));
    }
    let tree = scan(
        &fixture.0,
        &ScanOptions {
            initial_capacity: usize::MAX,
            max_entries: 2,
            ..Default::default()
        },
        &AtomicBool::new(false),
        &Progress::default(),
    )
    .unwrap();
    assert!(tree.truncated);
    assert_eq!(tree.nodes.len(), 2);
    assert_eq!(tree.nodes.capacity(), 2);
}

#[test]
fn nearby_rescans_keep_the_same_node_allocation_size() {
    let fixture = Fixture::new();
    for i in 0..4096 {
        fs::write(fixture.0.join(format!("file-{i}")), []).unwrap();
    }
    let first = fixture.scan();
    fs::write(fixture.0.join("one-more"), [1; 23]).unwrap();
    let next = scan(
        &fixture.0,
        &ScanOptions {
            initial_capacity: first.nodes.len(),
            ..Default::default()
        },
        &AtomicBool::new(false),
        &Progress::default(),
    )
    .unwrap();
    assert_eq!(next.nodes.len(), first.nodes.len() + 1);
    assert_eq!(next.nodes.capacity(), first.nodes.capacity());
    assert!(next.nodes.capacity() - next.nodes.len() < 4096);
    assert_eq!(next.nodes[0].files, first.nodes[0].files + 1);
    assert_eq!(next.nodes[0].apparent, first.nodes[0].apparent + 23);
}

#[test]
fn session_rescans_replace_only_on_success() {
    fn finish(session: &mut Session) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !session.poll() {
            assert!(std::time::Instant::now() < deadline, "scan did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    let fixture = Fixture::new();
    let mut session = Session::default();
    session.start(fixture.0.clone());
    finish(&mut session);
    let original = Arc::downgrade(&session.view.as_ref().unwrap().tree);
    session.view.as_mut().unwrap().options.apparent = true;
    session.start(fixture.0.clone());
    assert!(original.upgrade().is_some());
    session.cancel();
    assert!(original.upgrade().is_some());
    assert_eq!(session.error.as_deref(), Some("Scan cancelled"));
    session.start(fixture.0.join("missing"));
    finish(&mut session);
    assert!(session.error.is_some());
    assert!(original.upgrade().is_some());
    fs::write(fixture.0.join("new"), [1; 23]).unwrap();
    session.start(fixture.0.clone());
    finish(&mut session);
    assert!(session.error.is_none());
    assert!(original.upgrade().is_none());
    let view = session.view.as_ref().unwrap();
    assert!(view.options.apparent);
    assert_eq!(view.bytes[0], 7168 + 23);
    assert_eq!(view.files[0], 6);
}
#[test]
fn squarified_siblings_fill_without_overlapping() {
    let fixture = Fixture::new();
    let mut view = View::new(fixture.scan());
    view.options.depth = 1;
    view.options.apparent = true;
    view.rebuild();
    let tiles = view.tiles(100., 80., 1., 0.);
    let area: f64 = tiles.iter().map(|t| t.rect.w * t.rect.h).sum();
    assert!((area - 8000.).abs() < 0.001);
    for (i, a) in tiles.iter().enumerate() {
        assert!(
            a.rect.x >= 0.
                && a.rect.y >= 0.
                && a.rect.x + a.rect.w <= 100.001
                && a.rect.y + a.rect.h <= 80.001
        );
        for b in &tiles[i + 1..] {
            let overlap_x = (a.rect.x + a.rect.w).min(b.rect.x + b.rect.w) - a.rect.x.max(b.rect.x);
            let overlap_y = (a.rect.y + a.rect.h).min(b.rect.y + b.rect.h) - a.rect.y.max(b.rect.y);
            assert!(overlap_x < 0.001 || overlap_y < 0.001);
        }
    }
    let root = view.bytes[0] as f64;
    for tile in tiles {
        let fraction = tile.rect.w * tile.rect.h / 8000.;
        assert!((fraction - view.bytes[tile.id] as f64 / root).abs() < 0.001);
    }
}
#[test]
fn limits_cancellation_and_invalid_roots_are_reported() {
    let fixture = Fixture::new();
    let opts = ScanOptions {
        max_entries: 2,
        ..ScanOptions::default()
    };
    let tree = scan(
        &fixture.0,
        &opts,
        &AtomicBool::new(false),
        &Progress::default(),
    )
    .unwrap();
    assert!(tree.truncated);
    assert_eq!(tree.nodes.len(), 2);
    assert!(scan(
        &fixture.0,
        &opts,
        &AtomicBool::new(true),
        &Progress::default()
    )
    .is_err());
    assert!(scan(
        &fixture.0.join("empty"),
        &opts,
        &AtomicBool::new(false),
        &Progress::default()
    )
    .is_err());
}
#[test]
fn navigation_modes_marks_and_empty_layout() {
    let fixture = Fixture::new();
    let mut view = View::new(fixture.scan());
    let src = view
        .tree
        .nodes
        .iter()
        .position(|n| n.name == "src")
        .unwrap();
    view.selected = src;
    view.mark();
    view.enter();
    assert_eq!(view.zoom, src);
    view.up();
    assert_eq!(view.zoom, 0);
    assert!(view.marked.contains(&src));
    view.options.metric = Metric::Files;
    view.rebuild();
    assert_eq!(view.weights[0], 5.);
    view.options.filter = "nothing matches".into();
    view.rebuild();
    assert!(view.tiles(100., 50., 1., 0.).is_empty());
    assert!(view.tiles(0., 0., 1., 0.).is_empty());
}
#[cfg(unix)]
#[test]
fn symlinks_do_not_recurse_and_hard_links_count_once() {
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(&fixture.0, fixture.0.join("loop")).unwrap();
    fs::hard_link(fixture.0.join("src/main.rs"), fixture.0.join("copy.rs")).unwrap();
    let tree = fixture.scan();
    assert_eq!(tree.nodes[0].files, 6);
    let link = tree.nodes.iter().find(|n| n.name == "loop").unwrap();
    assert!(link.symlink && link.children.is_empty());
    assert_eq!(tree.nodes[0].apparent, 7168 + link.own_apparent);
    assert!(tree
        .nodes
        .iter()
        .any(|n| n.issue.as_ref().is_some_and(|s| s.contains("Hard link"))));
}

#[test]
fn parent_marks_absorb_children_and_age_changes_only_color() {
    let fixture = Fixture::new();
    let mut view = View::new(fixture.scan());
    let child = view
        .tree
        .nodes
        .iter()
        .position(|n| n.name == "main.rs")
        .unwrap();
    let parent = view.tree.nodes[child].parent.unwrap();
    view.selected = child;
    view.mark();
    view.selected = parent;
    view.mark();
    assert_eq!(view.marked.len(), 1);
    assert!(view.marked.contains(&parent));
    let expected = if view.tree.allocated_supported {
        view.tree.nodes[parent].allocated
    } else {
        view.tree.nodes[parent].apparent
    };
    assert_eq!(view.marked_bytes(), expected);
    let sizes = view.weights.clone();
    view.options.metric = Metric::Age;
    view.rebuild();
    assert_eq!(view.weights, sizes);
    view.selected = child;
    view.mark();
    assert!(view.marked.is_empty());
}

#[test]
fn copied_lookup_rules_respect_project_context() {
    assert_eq!(
        classify("OneDrive - Contoso", Category::Other).0,
        Category::Synced
    );
    assert_eq!(
        classify(".codex", Category::Other).0,
        Category::AgentScratch
    );
    assert_eq!(
        review_of("target", Category::Code, |name| name == "Cargo.toml"),
        Some(Review::Build)
    );
    assert_eq!(review_of("target", Category::Code, |_| false), None);
    assert_eq!(
        review_of("node_modules", Category::Code, |name| name
            == "package.json"),
        Some(Review::Other(Reclaim::Reinstallable))
    );
    assert_eq!(review_of("dist", Category::Code, |_| true), None);
}

#[test]
fn parallel_batches_preserve_paths_totals_and_progress() {
    let fixture = Fixture::new();
    for i in 0..24 {
        let directory = fixture.0.join(format!("src/branch-{i}"));
        fs::create_dir(&directory).unwrap();
        for j in 0..200 {
            fs::write(directory.join(format!("file-{j}.rs")), [42; 17]).unwrap();
        }
    }
    let run = |workers, max_entries| {
        let progress = Progress::default();
        let tree = scan(
            &fixture.0,
            &ScanOptions {
                workers,
                max_entries,
                ..Default::default()
            },
            &AtomicBool::new(false),
            &progress,
        )
        .unwrap();
        assert_eq!(
            progress.entries.load(Ordering::Relaxed) as usize,
            tree.nodes.len() - 1
        );
        assert_eq!(progress.files.load(Ordering::Relaxed), tree.nodes[0].files);
        assert_eq!(progress.dirs.load(Ordering::Relaxed), tree.nodes[0].dirs);
        tree
    };
    let serial = run(1, usize::MAX);
    let parallel = run(8, usize::MAX);
    let snapshot = |tree: &Tree| {
        tree.nodes
            .iter()
            .enumerate()
            .map(|(id, n)| {
                (
                    tree.path(id),
                    (
                        n.apparent,
                        n.allocated,
                        n.files,
                        n.dirs,
                        n.category,
                        n.review,
                        n.issue.clone(),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(snapshot(&serial), snapshot(&parallel));
    let limited = run(8, 137);
    assert_eq!(limited.nodes.len(), 137);
    assert!(limited.truncated);
    let shallow = scan(
        &fixture.0,
        &ScanOptions {
            workers: 8,
            max_depth: 1,
            ..Default::default()
        },
        &AtomicBool::new(false),
        &Progress::default(),
    )
    .unwrap();
    assert!(shallow.truncated);
    assert!(shallow.nodes.iter().skip(1).all(|n| n.children.is_empty()));
}

#[cfg(unix)]
#[test]
fn concurrent_directories_deduplicate_links_globally() {
    let fixture = Fixture::new();
    let original = fixture.0.join("src/main.rs");
    for i in 0..24 {
        let directory = fixture.0.join(format!("branch-{i}"));
        fs::create_dir(&directory).unwrap();
        fs::hard_link(&original, directory.join("linked.rs")).unwrap();
        std::os::unix::fs::symlink(&fixture.0, directory.join("loop")).unwrap();
    }
    let tree = fixture.scan();
    let links: Vec<_> = tree.nodes.iter().filter(|n| n.symlink).collect();
    assert_eq!(links.len(), 24);
    assert!(links.iter().all(|n| n.children.is_empty()));
    assert_eq!(tree.nodes[0].files, 5 + 24);
    assert_eq!(
        tree.nodes[0].apparent,
        7168 + links.iter().map(|n| n.own_apparent).sum::<u64>()
    );
    assert_eq!(
        tree.nodes
            .iter()
            .filter(|n| n.issue.as_deref() == Some("Hard link: size counted at another path"))
            .count(),
        24
    );
}

#[test]
fn cancellation_during_batched_scan_releases_workers() {
    let fixture = Fixture::new();
    for i in 0..4096 {
        fs::write(fixture.0.join(format!("file-{i}")), []).unwrap();
    }
    let cancel = AtomicBool::new(false);
    let progress = Progress::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while progress.entries.load(Ordering::Relaxed) == 0
                && std::time::Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            cancel.store(true, Ordering::Relaxed);
        });
        let result = scan(&fixture.0, &ScanOptions::default(), &cancel, &progress);
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    });
}
