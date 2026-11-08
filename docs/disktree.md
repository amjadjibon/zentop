# DiskTree

DiskTree is a read-only disk-usage explorer for Zentop's GUI and TUI, inspired by
[tobi/disktree](https://github.com/tobi/disktree). Both interfaces use the same
scanner, category rules, squarified treemap layout and selection model. The
required upstream squarification, category/reclaim lookups and size/count
formatters are copied under the [MIT license](../licenses/disktree-MIT.md);
[UPSTREAM.md](../crates/zentop-core/src/disktree/upstream/UPSTREAM.md) records the
source revision and extraction boundaries. The GUI also copies the upstream
treemap painter, theme-aware palette, breadcrumbs, selection figures, review
rows, meters, legend swatches and key-hint widgets. Their source and adaptations
are listed in the [UI provenance](../crates/zentop-gui/src/ui/disktree/upstream/UPSTREAM.md).

## Open it

**GUI:** choose **View → DiskTree…**, press **⌘⇧D / Ctrl+Shift+D**, or open
Storage details and click **Open DiskTree…**. Choose a directory with the
native folder picker. A scan starts only after you choose a folder.
The themed window top bar contains folder navigation, breadcrumbs, view modes,
hidden/apparent-size options, depth and rescan/cancel. On narrower windows, view
settings occupy a second header row.

**TUI:** press **d** in the monitor to scan the current working directory, or:

```sh
cargo run -p zentop-tui -- --disk /path/to/folder
```

In DiskTree, **v** chooses another path; **M** returns to the monitor.

## Read the map

- **Size:** area represents the measured bytes.
- **Files:** area represents file counts, including zero-byte files.
- **Age:** area still represents bytes; color represents the newest write in
  each subtree. The GUI uses the upstream week/month/six-month/year/older
  age buckets; the TUI labels its age bands in the legend.
- Otherwise colors distinguish code, agent scratch, toolchains, synced data,
  git, media, documents, caches and unclassified data.
- Diagonal hatching highlights cache/build-output candidates, including their
  descendants. It is a heuristic for review, not a statement that removal is safe.
- **Hidden files** controls visibility, including whole hidden subtrees. The
  initial scan includes them. **Apparent size** switches from allocated blocks
  to logical file length. On platforms without allocated-block support,
  apparent size is used and the GUI's toggle is disabled.

The selection panel shows a path, measured size, share of the current view,
file count, last write and kind. **Worth a look** lists large review candidates.
The upstream lookup checks for a sibling project manifest before classifying
`target` or `node_modules` as review candidates; generic `build` and `dist` names
are not enough on their own.
Git worktrees may contain uncommitted work; no git status inspection is performed.

## Navigate and mark

In the GUI, click a tile to select it and double-click to enter a directory.
Click a breadcrumb or **Up** to go back. The treemap accepts keyboard shortcuts
after it is clicked. Controls also work through the toolbar and sidebar.

| Key | Action |
| --- | --- |
| Arrows / hjkl | Move between displayed tiles |
| Enter | Enter the selected directory |
| Backspace / Esc | Up; in the TUI, Esc first clears a filter or cancels a scan |
| Space / x | Mark or unmark for review |
| c | Review marked paths |
| t | Switch Size / Files / Age |
| i | Include/exclude hidden entries |
| d | Switch allocated/apparent bytes |
| [ / ] | Reduce/increase visible depth (1–8) |
| / | Filter by name; Enter applies the text |
| 0 | Reset to the scanned root |
| r | Rescan |
| v | Choose a new directory |

Additional TUI keys: **Tab** selects the next sibling; **w** cycles Worth a
look suggestions; **?** shows keys; **q / Ctrl+C** quits. In its marked review,
Space unmarks, **!** clears all marks, and Enter opens the selected directory.
**Ctrl+O** also opens the path prompt. **Esc** at the root returns to the monitor.

Marking a parent absorbs marks beneath it; totals do not double-count nested
marks. Marks are only an in-memory review list. There are no trash, permanent
delete, shell-script export or remote SSH operations in this implementation.
No future free-space saving is promised: shared extents and filesystem snapshots
can make allocated sizes overlap.

## Scan boundaries and incomplete results

A coordinator dispatches directory reads to up to eight filesystem workers.
Workers return bounded batches of 128 entries; the coordinator owns node IDs,
entry limits, inode deduplication and totals. GUI and TUI share this scanner.
The workers read directory entries and metadata, never file contents. Child
symlinks are represented as links and are not traversed. On Unix, hard-linked
files are counted once and directories on another device are skipped. Other
platforms currently use apparent sizes and do not deduplicate hard links.

Scans are bounded to **1,000,000 entries** (including the root) and **128 directory
levels**. Limit hits produce a clearly labeled **partial scan**, not a complete
usage estimate. Select a smaller folder to inspect it fully. Unreadable paths,
changed files and skipped filesystem boundaries are counted and reported.

Scans can be cancelled; replacing a scan cancels the previous worker. An OS call
blocked on a slow filesystem may take time to return before cancellation is seen.
The existing completed view stays visible while a rescan runs. Free capacity is
sampled with the scan, rather than continuously refreshed.

The GUI shapes at most 150 labels per frame. The renderer draws at most 5,000 rectangles and omits subpixel tiles; entering a
directory reveals its children at a larger scale. Terminal cells use a corrected
aspect ratio so their treemap geometry resembles the GUI's pixel layout.

## Upstream license

The full notice is available in **About Zentop → DiskTree components · MIT
license**, and in the terminal with `cargo run -p zentop-tui -- --disk-license`.
The notice is embedded in the GUI and TUI binaries as well as retained in the
source repository. No upstream deletion, trash, export, or SSH stack is copied.
