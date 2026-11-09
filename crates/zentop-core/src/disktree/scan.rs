use super::{classify, Category, Node, Tree, Volume};
use std::{
    collections::{HashSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Instant,
};

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub max_entries: usize,
    pub max_depth: usize,
    pub same_filesystem: bool,
    /// Concurrent directory readers, bounded to 1–8 to avoid saturating the disk.
    pub workers: usize,
    /// Expected entries from a previous scan. Zero grows on demand.
    /// Reservation includes modest growth headroom and is capped by `max_entries`.
    pub initial_capacity: usize,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            initial_capacity: 0,
            max_entries: 1_000_000,
            max_depth: 128,
            same_filesystem: true,
            workers: thread::available_parallelism().map_or(1, |n| n.get().min(8)),
        }
    }
}
#[derive(Default)]
pub struct Progress {
    pub entries: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
}

pub struct ScanJob {
    pub progress: Arc<Progress>,
    pub root: PathBuf,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Tree, String>>,
}
impl ScanJob {
    pub fn start(root: PathBuf) -> io::Result<Self> {
        Self::with_previous(root, None)
    }
    pub(super) fn with_previous(root: PathBuf, previous: Option<&Tree>) -> io::Result<Self> {
        let progress = Arc::new(Progress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, result) = mpsc::sync_channel(1);
        let target = root.clone();
        let token = cancel.clone();
        let counters = progress.clone();
        let previous = previous.map(|tree| (tree.root.clone(), tree.nodes.len()));
        thread::Builder::new()
            .name("zentop-disktree".into())
            .spawn(move || {
                // Resolve aliases/relative paths off the UI thread. A different
                // folder must not inherit a large previous folder's reservation.
                let initial_capacity = previous
                    .filter(|(root, _)| fs::canonicalize(&target).is_ok_and(|p| p == *root))
                    .map_or(0, |(_, len)| len);
                let options = ScanOptions {
                    initial_capacity,
                    ..Default::default()
                };
                let result = scan(&target, &options, &token, &counters).map_err(|e| e.to_string());
                let _ = tx.send(result);
            })?;
        Ok(Self {
            progress,
            root,
            cancel,
            result,
        })
    }
    pub fn poll(&self) -> Option<Result<Tree, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("Disk scan worker stopped".into())),
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl Drop for ScanJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(unix)]
fn allocated(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}
#[cfg(not(unix))]
fn allocated(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}
#[cfg(unix)]
fn file_id(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}
#[cfg(not(unix))]
fn file_id(_metadata: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}
#[cfg(unix)]
fn linked(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() > 1
}
#[cfg(not(unix))]
fn linked(_metadata: &fs::Metadata) -> bool {
    false
}

struct Directory {
    id: usize,
    path: PathBuf,
    depth: usize,
}
struct Entry {
    name: std::ffi::OsString,
    path: PathBuf,
    metadata: io::Result<fs::Metadata>,
}
struct Batch {
    parent: usize,
    depth: usize,
    entries: Vec<Entry>,
    done: bool,
}

// Use the same allocation size for nearby entry counts, so small changes between
// rescans do not leave a differently sized large block in the allocator each time.
fn node_capacity(entries: usize) -> usize {
    const CHUNK: usize = 4096;
    if entries < CHUNK {
        entries
    } else {
        entries.saturating_add((CHUNK - entries % CHUNK) % CHUNK)
    }
}

// Workers only perform I/O. The coordinator owns all IDs, limits and inode sets.
fn read_directory(
    directory: Directory,
    output: &mpsc::SyncSender<Batch>,
    cancel: &AtomicBool,
    stop: &AtomicBool,
) {
    let mut batch = Batch {
        parent: directory.id,
        depth: directory.depth + 1,
        entries: Vec::with_capacity(128),
        done: false,
    };
    match fs::read_dir(&directory.path) {
        Ok(entries) => {
            for entry in entries {
                if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                    return;
                }
                let entry = match entry {
                    Ok(entry) => {
                        let path = entry.path();
                        Entry {
                            name: entry.file_name(),
                            metadata: fs::symlink_metadata(&path),
                            path,
                        }
                    }
                    Err(error) => Entry {
                        name: Default::default(),
                        path: directory.path.clone(),
                        metadata: Err(error),
                    },
                };
                batch.entries.push(entry);
                if batch.entries.len() == 128 {
                    let next = Batch {
                        parent: batch.parent,
                        depth: batch.depth,
                        entries: Vec::with_capacity(128),
                        done: false,
                    };
                    if output.send(std::mem::replace(&mut batch, next)).is_err() {
                        return;
                    }
                }
            }
        }
        Err(error) => batch.entries.push(Entry {
            name: Default::default(),
            path: directory.path,
            metadata: Err(error),
        }),
    }
    batch.done = true;
    let _ = output.send(batch);
}
fn issue(tree: &mut Tree, path: &Path, error: impl std::fmt::Display) {
    tree.skipped += 1;
    if tree.errors.len() < 20 {
        tree.errors.push(format!("{}: {error}", path.display()));
    }
}
fn node(
    name: std::ffi::OsString,
    parent: Option<usize>,
    metadata: &fs::Metadata,
    category: Category,
) -> Node {
    let is_dir = metadata.is_dir();
    let text = name.to_string_lossy();
    let (category, review) = classify(&text, category);
    Node {
        hidden: text.starts_with('.'),
        name,
        parent,
        children: Vec::new(),
        is_dir,
        symlink: metadata.is_symlink(),
        apparent: 0,
        allocated: 0,
        files: 0,
        dirs: 0,
        modified: metadata.modified().ok(),
        category,
        review,
        issue: None,
        own_files: u64::from(!is_dir),
        own_apparent: if is_dir { 0 } else { metadata.len() },
        own_allocated: allocated(metadata),
    }
}
/// Scan without following child symlinks. Hard links count once on Unix.
/// Errors, mount/depth boundaries and caps are reported rather than treated as complete data.
pub fn scan(
    root: &Path,
    options: &ScanOptions,
    cancel: &AtomicBool,
    progress: &Progress,
) -> io::Result<Tree> {
    let started = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Scan cancelled"));
    }
    let root = fs::canonicalize(root)?;
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a directory to scan",
        ));
    }
    // Preserve the immediate error for an unreadable root.
    drop(fs::read_dir(&root)?);
    let root_device = file_id(&metadata).0;
    let name = root
        .file_name()
        .map_or_else(|| root.as_os_str().to_owned(), std::ffi::OsStr::to_owned);
    // Avoid repeatedly growing/copying a large node vector. Allow small changes
    // since the last scan without doubling its allocation on the next push.
    let capacity = node_capacity(options.initial_capacity).min(options.max_entries.max(1));
    let mut nodes = Vec::with_capacity(capacity);
    nodes.push(node(name, None, &metadata, Category::Other));
    let mut tree = Tree {
        nodes,
        root: root.clone(),
        volume: None,
        skipped: 0,
        errors: Vec::new(),
        truncated: false,
        allocated_supported: cfg!(unix),
        elapsed: Default::default(),
    };
    let mut pending = VecDeque::from([Directory {
        id: 0,
        path: root.clone(),
        depth: 0,
    }]);
    let mut links = HashSet::new();
    let mut directories = HashSet::new();
    if cfg!(unix) {
        directories.insert(file_id(&metadata));
    }
    let stop = AtomicBool::new(false);
    let traversal = thread::scope(|scope| -> io::Result<()> {
        let workers = options.workers.clamp(1, 8);
        let (jobs, receiver) = mpsc::channel::<Directory>();
        let receiver = Arc::new(Mutex::new(receiver));
        let (output, batches) = mpsc::sync_channel::<Batch>(workers * 2);
        let mut run = |tree: &mut Tree| -> io::Result<()> {
            let mut active = 0;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "Scan cancelled"));
                }
                if tree.nodes.len() >= options.max_entries.max(1) {
                    tree.truncated = true;
                    break;
                }
                while active < workers {
                    let Some(directory) = pending.pop_front() else {
                        break;
                    };
                    jobs.send(directory)
                        .map_err(|_| io::Error::other("Disk scan worker stopped"))?;
                    active += 1;
                }
                if active == 0 {
                    break;
                }
                let batch = match batches.recv_timeout(std::time::Duration::from_millis(20)) {
                    Ok(batch) => batch,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => return Err(io::Error::other("Disk scan worker stopped")),
                };
                if batch.done {
                    active -= 1;
                }
                let parent = batch.parent;
                let depth = batch.depth;
                for entry in batch.entries {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "Scan cancelled"));
                    }
                    if tree.nodes.len() >= options.max_entries.max(1) {
                        tree.truncated = true;
                        break;
                    }
                    let path = entry.path;
                    let metadata = match entry.metadata {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            if path == tree.path(parent) {
                                tree.nodes[parent].issue = Some(error.to_string());
                            }
                            issue(tree, &path, error);
                            continue;
                        }
                    };
                    let id = tree.nodes.len();
                    let mut child = node(
                        entry.name,
                        Some(parent),
                        &metadata,
                        tree.nodes[parent].category,
                    );
                    if !child.is_dir
                        && !child.symlink
                        && linked(&metadata)
                        && !links.insert(file_id(&metadata))
                    {
                        child.own_files = 0;
                        child.own_apparent = 0;
                        child.own_allocated = 0;
                        child.issue = Some("Hard link: size counted at another path".into());
                    }
                    if child.is_dir {
                        let parent_path = path.parent().unwrap_or(root.as_path());
                        child.review = super::review_of(
                            &child.name.to_string_lossy(),
                            tree.nodes[parent].category,
                            |wanted| parent_path.join(wanted).exists(),
                        );
                    } else {
                        child.category = tree.nodes[parent].category;
                        child.review = None;
                    }
                    let descend = child.is_dir && !child.symlink;
                    let reason = if descend
                        && cfg!(unix)
                        && options.same_filesystem
                        && file_id(&metadata).0 != root_device
                    {
                        Some("Different filesystem; not scanned")
                    } else if descend && depth >= options.max_depth {
                        tree.truncated = true;
                        Some("Scan depth limit reached")
                    } else if descend && cfg!(unix) && !directories.insert(file_id(&metadata)) {
                        Some("Directory already visited")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        child.issue = Some(reason.into());
                        child.own_allocated = 0;
                        issue(tree, &path, reason);
                    }
                    progress.entries.fetch_add(1, Ordering::Relaxed);
                    progress
                        .bytes
                        .fetch_add(child.own_apparent, Ordering::Relaxed);
                    if child.is_dir {
                        progress.dirs.fetch_add(1, Ordering::Relaxed);
                    } else {
                        progress.files.fetch_add(child.own_files, Ordering::Relaxed);
                    }
                    tree.nodes[parent].children.push(id);
                    tree.nodes.push(child);
                    if descend && reason.is_none() {
                        pending.push_back(Directory { id, path, depth });
                    }
                }
            }
            Ok(())
        };
        for index in 0..workers {
            let receiver = receiver.clone();
            let output = output.clone();
            let stop = &stop;
            thread::Builder::new()
                .name(format!("zentop-disk-{index}"))
                .spawn_scoped(scope, move || loop {
                    let job = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok(directory) = job else {
                        return;
                    };
                    if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                        return;
                    }
                    read_directory(directory, &output, cancel, stop);
                })
                .inspect_err(|_| {
                    stop.store(true, Ordering::Relaxed);
                })?;
        }
        drop(output);
        let result = run(&mut tree);
        stop.store(true, Ordering::Relaxed);
        // Disconnect before joining, releasing producers blocked on a full batch channel.
        drop(batches);
        drop(jobs);
        result
    });
    traversal?;
    for i in (0..tree.nodes.len()).rev() {
        let (apparent, allocated, files, dirs, modified, parent) = {
            let n = &mut tree.nodes[i];
            n.apparent = n.apparent.saturating_add(n.own_apparent);
            n.allocated = n.allocated.saturating_add(n.own_allocated);
            n.files = n.files.saturating_add(n.own_files);
            n.dirs = n.dirs.saturating_add(u64::from(n.is_dir && i != 0));
            (
                n.apparent,
                n.allocated,
                n.files,
                n.dirs,
                n.modified,
                n.parent,
            )
        };
        if let Some(parent) = parent {
            let p = &mut tree.nodes[parent];
            p.apparent = p.apparent.saturating_add(apparent);
            p.allocated = p.allocated.saturating_add(allocated);
            p.files = p.files.saturating_add(files);
            p.dirs = p.dirs.saturating_add(dirs);
            p.modified = p.modified.max(modified);
        }
    }
    // Infer an otherwise unknown directory from its largest recognizable child.
    for id in (0..tree.nodes.len()).rev() {
        if tree.nodes[id].category == Category::Other {
            if let Some(category) = tree.nodes[id]
                .children
                .iter()
                .filter(|i| tree.nodes[**i].category != Category::Other)
                .max_by_key(|i| tree.nodes[**i].apparent)
                .map(|i| tree.nodes[*i].category)
            {
                tree.nodes[id].category = category;
            }
        }
    }
    for id in 1..tree.nodes.len() {
        if tree.nodes[id].category == Category::Other {
            if let Some(parent) = tree.nodes[id].parent {
                tree.nodes[id].category = tree.nodes[parent].category;
            }
        }
    }
    // Completed trees live across rescans; discard traversal growth slack.
    for node in &mut tree.nodes {
        node.children.shrink_to_fit();
    }
    // Keep the small rescan headroom: shrinking/reallocating this large block
    // on every scan makes the allocator retain more memory than the slack saves.
    tree.nodes.shrink_to(node_capacity(tree.nodes.len()));
    let disks = sysinfo::Disks::new_with_refreshed_list();
    tree.volume = disks
        .iter()
        .filter(|d| root.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().components().count())
        .map(|d| Volume {
            mount: d.mount_point().to_path_buf(),
            total: d.total_space(),
            available: d.available_space(),
        });
    tree.elapsed = started.elapsed();
    Ok(tree)
}
