# DiskTree scan performance

Measured on macOS, 2026-11-08, with optimized (`--release`) binaries. The target
was the user's `/Users/amjadhossain/github.com/amjadjibon` directory, bringing
DiskTree closer to the locally installed `dust` without dropping scanned data.

## Findings and change

A three-second `/usr/bin/sample` profile of the original scanner had 2,344
main-thread samples: approximately 51% in `lstat`, 34% reading directory entries,
and 13% opening directories. These filesystem calls ran serially. `dust` with
one thread also took about eight seconds, while its default traversal was much
faster. View construction took only about four milliseconds.

The shared scanner now dispatches directories to 1–8 I/O workers (bounded by
available parallelism). Each worker returns at most 128 entries per batch over
a bounded result channel. A single coordinator owns node IDs, hard-link and
directory inode sets, progress, classifications, errors and scan limits. All
parents still precede their children, allowing the same aggregation algorithm.
Dropping the result channel releases blocked producers on cancellation or caps.
No new dependencies or content reads are introduced.

## Results

Five repeated real-directory scans, followed by five local `dust -x -n 1` runs:

| Scanner | Median | Observed range |
| --- | ---: | ---: |
| Original DiskTree | 8.912 s | 7.928–9.011 s |
| Parallel DiskTree | 2.030 s | 1.786–2.482 s |
| dust, default threads | 1.686 s | 1.601–1.862 s |

DiskTree's median scan time fell by **77.2% (4.39× faster)**. These are local,
mostly warm-cache measurements, not cold-cache or tail-latency guarantees.
DiskTree timing excludes process startup and the separate view construction;
`dust` timing includes its process and output formatting (output redirected).
Different tools are not guaranteed to have identical accounting semantics.
The working directory can change; a final one-run before/after comparison
matched all counts and totals exactly: 386,121 nodes, 271,162 deduplicated files,
75,102 directories, 31,493,361,432 apparent bytes and 32,188,456,960 allocated
bytes, with no skipped entries or truncation.

A fixed synthetic tree (51,000 files, 3,000 directories) also retained identical
counts and byte totals: median **129.420 ms → 59.276 ms**, seven runs each.
The synthetic fixture contained 500 project folders, each with five nested
subdirectories holding 20 128-byte files, plus Cargo.toml and package.json.

Memory tradeoff, measured with `/usr/bin/time -l`: single-run maximum RSS grew
from **149.9 MiB to 159.5 MiB**. Over five scans in one benchmark process the
maximum was **229.2 MiB → 346.4 MiB**; allocator retention and traversal order
increase the process high-water mark. The pool and in-flight batches are
bounded; the admitted tree and pending directory queue scale with entry count.

## Reproduce and verify

```sh
cargo run --release -p zentop-core --example disktree_scan -- /path/to/folder 5
/usr/bin/time -l target/release/examples/disktree_scan /path/to/folder 1
/usr/bin/time -p dust -x -n 1 /path/to/folder
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

The benchmark emits scan/view timings and totals as CSV. Retain a copy of the
original executable before changes for paired comparisons. Avoid builds or
other writes beneath the scanned directory during measurement.

Added regression coverage compares one versus eight workers across multiple
batches and checks global hard-link deduplication, symlink loops, progress,
exact caps, depth boundaries and cancellation after work starts. All 96
workspace tests and Clippy passed. Directory iteration remains serial within
a single directory, so a very wide folder may benefit less than a repository
tree. Blocked filesystem calls can still delay worker shutdown, as before;
frontends continue to cancel without joining the scan thread.
