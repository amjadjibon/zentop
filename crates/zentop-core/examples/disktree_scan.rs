//! Release-build scanner benchmark. Usage: disktree_scan <directory> [runs] [--retain].
//! --retain models UI rescans: retain the old view during scanning, then replace it.
//! tree_heap_bytes estimates owned tree allocations (excluding allocator overhead).
//! rss_kib samples process RSS with ps on Unix; zero means unavailable.
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};
use zentop_core::disktree::{scan, Progress, ScanOptions, View};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or("usage: disktree_scan <directory> [runs] [--retain]")?,
    );
    let runs: usize = args
        .next()
        .map(|n| n.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(5);
    if runs == 0 {
        return Err("runs must be positive".into());
    }
    let retain = args.next().is_some_and(|arg| arg == "--retain");
    let mut retained: Option<View> = None;
    println!("run,scan_ms,view_ms,nodes,files,dirs,apparent,allocated,skipped,truncated,tree_heap_bytes,rss_kib");
    for run in 1..=runs {
        let tree = scan(
            &root,
            &ScanOptions {
                initial_capacity: retained.as_ref().map_or(0, |v| v.tree.nodes.len()),
                ..Default::default()
            },
            &AtomicBool::new(false),
            &Progress::default(),
        )?;
        let scan_ms = tree.elapsed.as_secs_f64() * 1000.;
        let start = Instant::now();
        let view = if let Some(mut previous) = retained.take() {
            previous.replace_tree(tree);
            previous
        } else {
            View::new(tree)
        };
        let view_ms = start.elapsed().as_secs_f64() * 1000.;
        let tree_heap = view.tree.nodes.capacity()
            * std::mem::size_of::<zentop_core::disktree::Node>()
            + view
                .tree
                .nodes
                .iter()
                .map(|n| {
                    n.name.capacity()
                        + n.children.capacity() * std::mem::size_of::<usize>()
                        + n.issue.as_ref().map_or(0, |s| s.capacity())
                })
                .sum::<usize>();
        let rss = resident_kib();
        let root = &view.tree.nodes[0];
        println!(
            "{run},{scan_ms:.3},{view_ms:.3},{},{},{},{},{},{},{},{tree_heap},{rss}",
            view.tree.nodes.len(),
            root.files,
            root.dirs,
            root.apparent,
            root.allocated,
            view.tree.skipped,
            view.tree.truncated
        );
        std::hint::black_box(&view);
        if retain {
            retained = Some(view);
        }
    }
    std::hint::black_box(retained);
    Ok(())
}

fn resident_kib() -> u64 {
    #[cfg(unix)]
    if let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
    {
        return String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap_or(0);
    }
    0
}
