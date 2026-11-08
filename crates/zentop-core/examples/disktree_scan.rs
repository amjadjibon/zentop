//! Release-build scanner benchmark. Usage: disktree_scan <directory> [runs].
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};
use zentop_core::disktree::{scan, Progress, ScanOptions, View};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or("usage: disktree_scan <directory> [runs]")?,
    );
    let runs: usize = args
        .next()
        .map(|n| n.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(5);
    if runs == 0 {
        return Err("runs must be positive".into());
    }
    println!("run,scan_ms,view_ms,nodes,files,dirs,apparent,allocated,skipped,truncated");
    for run in 1..=runs {
        let tree = scan(
            &root,
            &ScanOptions::default(),
            &AtomicBool::new(false),
            &Progress::default(),
        )?;
        let scan_ms = tree.elapsed.as_secs_f64() * 1000.;
        let start = Instant::now();
        let view = View::new(tree);
        let view_ms = start.elapsed().as_secs_f64() * 1000.;
        let root = &view.tree.nodes[0];
        println!(
            "{run},{scan_ms:.3},{view_ms:.3},{},{},{},{},{},{},{}",
            view.tree.nodes.len(),
            root.files,
            root.dirs,
            root.apparent,
            root.allocated,
            view.tree.skipped,
            view.tree.truncated
        );
        std::hint::black_box(&view);
    }
    Ok(())
}
