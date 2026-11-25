//! Who may use the proxy without the session token: the process families of
//! apps Zentop launched in this capture session (phase 3D).
//!
//! A family is keyed by its root's macOS unique process ID, which is never
//! reused while the Mac is up, so a recycled PID can't join a family.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time;

use crate::Attribution;

/// How far up the parent chain a descendant is looked for.
const MAX_DEPTH: usize = 16;
/// How long an unauthorized request waits for a launch in progress.
pub(crate) const LAUNCH_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub pid: u32,
    pub ppid: u32,
    pub unique_id: u64,
    pub parent_unique_id: u64,
    /// Start time as (seconds, microseconds) since the epoch.
    pub started: (u64, u64),
}

/// Read access to live processes, replaceable in tests.
pub(crate) trait ProcessTable: Send + Sync {
    fn identity(&self, pid: u32) -> Option<Identity>;
    /// The process macOS holds responsible for `pid`.
    fn responsible(&self, pid: u32) -> Option<u32>;
    fn executable(&self, pid: u32) -> Option<PathBuf>;
}

/// The live process table.
pub(crate) struct System;

#[cfg(target_os = "macos")]
impl ProcessTable for System {
    fn identity(&self, pid: u32) -> Option<Identity> {
        #[derive(Default)]
        #[repr(C)]
        struct Raw {
            unique_id: u64,
            parent_unique_id: u64,
            start_sec: u64,
            start_usec: u64,
            pid: i32,
            ppid: i32,
        }
        unsafe extern "C" {
            fn zentop_process_identity(pid: i32, out: *mut Raw) -> i32;
        }
        let pid = i32::try_from(pid).ok()?;
        let mut raw = Raw::default();
        // The C helper writes one `Raw` through the live pointer.
        if unsafe { zentop_process_identity(pid, &mut raw) } != 1 || raw.pid != pid {
            return None;
        }
        Some(Identity {
            pid: raw.pid as u32,
            ppid: u32::try_from(raw.ppid).unwrap_or(0),
            unique_id: raw.unique_id,
            parent_unique_id: raw.parent_unique_id,
            started: (raw.start_sec, raw.start_usec),
        })
    }

    fn responsible(&self, pid: u32) -> Option<u32> {
        unsafe extern "C" {
            fn zentop_responsible_pid(pid: i32) -> i32;
        }
        // Takes and returns plain integers; an unknown PID yields -1.
        let owner = unsafe { zentop_responsible_pid(i32::try_from(pid).ok()?) };
        u32::try_from(owner).ok().filter(|&owner| owner > 0)
    }

    fn executable(&self, pid: u32) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        unsafe extern "C" {
            fn zentop_process_path(pid: i32, buffer: *mut u8, size: u32) -> i32;
        }
        // PROC_PIDPATHINFO_MAXSIZE.
        let mut buffer = [0u8; 4096];
        // Writes at most `size` bytes into the live buffer.
        let length = unsafe {
            zentop_process_path(
                i32::try_from(pid).ok()?,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        };
        let length = usize::try_from(length).ok().filter(|&length| length > 0)?;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(
            &buffer[..length.min(buffer.len())],
        )))
    }
}

#[cfg(not(target_os = "macos"))]
impl ProcessTable for System {
    fn identity(&self, _: u32) -> Option<Identity> {
        None
    }
    fn responsible(&self, _: u32) -> Option<u32> {
        None
    }
    fn executable(&self, _: u32) -> Option<PathBuf> {
        None
    }
}

/// The macOS unique process ID of a live process, for callers that watch a
/// launched app after capture stops.
pub fn process_unique_id(pid: u32) -> Option<u64> {
    System.identity(pid).map(|identity| identity.unique_id)
}

/// An app Zentop launched, whose process family may use the proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchedApp {
    pub id: u64,
    pub name: String,
    pub pid: u32,
    pub unique_id: u64,
    pub bundle: PathBuf,
}

/// A family member, as the engine caches it per connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub id: u64,
    pub app: Arc<str>,
}

struct Family {
    app: LaunchedApp,
    label: Arc<str>,
    root: Identity,
}

#[derive(Default)]
struct State {
    families: Vec<Family>,
    next_id: u64,
    pending: usize,
}

pub(crate) struct Families<T: ProcessTable = System> {
    table: T,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Default for Families<System> {
    fn default() -> Self {
        Self::new(System)
    }
}

fn inside(path: &Path, bundle: &Path) -> bool {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    path.starts_with(bundle)
}

impl<T: ProcessTable> Families<T> {
    pub(crate) fn new(table: T) -> Self {
        Self {
            table,
            state: Mutex::default(),
            changed: watch::channel(0).0,
        }
    }

    fn notify(&self) {
        self.changed.send_modify(|generation| *generation += 1);
    }

    fn alive(&self, root: &Identity) -> bool {
        self.table
            .identity(root.pid)
            .is_some_and(|now| now.unique_id == root.unique_id)
    }

    /// Drops families whose root exited. Returns whether any were dropped.
    fn prune(&self, state: &mut State) -> bool {
        let before = state.families.len();
        state.families.retain(|family| self.alive(&family.root));
        state.families.len() != before
    }

    pub(crate) fn register(
        &self,
        pid: u32,
        name: &str,
        bundle: &Path,
    ) -> Result<LaunchedApp, String> {
        let bundle = std::fs::canonicalize(bundle)
            .map_err(|error| format!("Could not read {}: {error}", bundle.display()))?;
        let exited = || format!("{name} exited before Zentop could register it");
        let root = self.table.identity(pid).ok_or_else(exited)?;
        let executable = self.table.executable(pid).ok_or_else(exited)?;
        if !inside(&executable, &bundle) {
            return Err(format!("PID {pid} is not running {name}"));
        }
        // The PID still belongs to the process that was checked.
        if !self.alive(&root) {
            return Err(exited());
        }
        let mut state = self.state.lock().unwrap();
        self.prune(&mut state);
        if let Some(family) = state
            .families
            .iter()
            .find(|family| family.root.unique_id == root.unique_id)
        {
            return Ok(family.app.clone());
        }
        let id = state.next_id;
        state.next_id += 1;
        let app = LaunchedApp {
            id,
            name: name.to_owned(),
            pid,
            unique_id: root.unique_id,
            bundle,
        };
        state.families.push(Family {
            app: app.clone(),
            label: name.into(),
            root,
        });
        drop(state);
        self.notify();
        Ok(app)
    }

    pub(crate) fn forget(&self, id: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        let before = state.families.len();
        state.families.retain(|family| family.app.id != id);
        let removed = state.families.len() != before;
        drop(state);
        if removed {
            self.notify();
        }
        removed
    }

    pub(crate) fn launched(&self) -> Vec<LaunchedApp> {
        let mut state = self.state.lock().unwrap();
        self.prune(&mut state);
        state
            .families
            .iter()
            .map(|family| family.app.clone())
            .collect()
    }

    pub(crate) fn contains(&self, id: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        self.prune(&mut state);
        state.families.iter().any(|family| family.app.id == id)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.state.lock().unwrap().families.is_empty()
    }

    /// Whether any family is registered or being launched. Without either,
    /// requests without the token are refused at once, as before phase 3D.
    pub(crate) fn admits_launched(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.families.is_empty() || state.pending > 0
    }

    fn begin_launch(&self) {
        self.state.lock().unwrap().pending += 1;
    }

    fn end_launch(&self) {
        let mut state = self.state.lock().unwrap();
        state.pending = state.pending.saturating_sub(1);
        drop(state);
        self.notify();
    }

    /// The family `owner` belongs to. Unknown attribution never matches.
    pub(crate) fn member(&self, owner: &Attribution) -> Option<Member> {
        let Attribution::Known { pid, unique_id, .. } = owner else {
            return None;
        };
        let mut state = self.state.lock().unwrap();
        self.prune(&mut state);
        if state.families.is_empty() {
            return None;
        }
        let me = self
            .table
            .identity(*pid)
            .filter(|me| me.unique_id == *unique_id)?;
        let family = self.relate(&state.families, &me)?;
        let executable = self.table.executable(me.pid)?;
        if !inside(&executable, &family.app.bundle) {
            return None;
        }
        // Every lookup above used the PID; it must still name the same process.
        self.table
            .identity(me.pid)
            .filter(|again| again.unique_id == me.unique_id)?;
        Some(Member {
            id: family.app.id,
            app: family.label.clone(),
        })
    }

    fn relate<'a>(&self, families: &'a [Family], me: &Identity) -> Option<&'a Family> {
        let root = |unique_id: u64| {
            families
                .iter()
                .find(|family| family.root.unique_id == unique_id)
        };
        if let Some(family) = root(me.unique_id) {
            return Some(family);
        }
        // Each step compares the recorded parent unique ID with the parent's
        // actual one, so a reused PID ends the walk instead of joining it.
        let mut current = *me;
        for _ in 0..MAX_DEPTH {
            if let Some(family) = root(current.parent_unique_id) {
                return Some(family);
            }
            if current.ppid <= 1 {
                break;
            }
            match self.table.identity(current.ppid) {
                Some(parent) if parent.unique_id == current.parent_unique_id => current = parent,
                _ => break,
            }
        }
        let responsible = self
            .table
            .responsible(me.pid)
            .filter(|&pid| pid != me.pid)?;
        let owner = self.table.identity(responsible)?;
        // A process can't be responsible for one that started before it, so
        // a root that reused the responsible PID later doesn't match.
        root(owner.unique_id).filter(|_| owner.started <= me.started)
    }

    /// Waits until a launch in progress registers something or ends.
    /// Returns `false` when no launch is pending or the grace period ended.
    pub(crate) async fn wait_for_launch(&self, deadline: time::Instant) -> bool {
        let mut changes = self.changed.subscribe();
        if self.state.lock().unwrap().pending == 0 {
            return false;
        }
        matches!(
            time::timeout_at(deadline, changes.changed()).await,
            Ok(Ok(()))
        )
    }
}

/// A cloneable handle the GUI uses to register the apps it launches. It may
/// outlive the session; registrations then have no effect.
#[derive(Clone)]
pub struct LaunchAccess(pub(crate) Arc<Families>);

impl LaunchAccess {
    /// Call before launching. While a guard is alive, unauthorized requests
    /// wait briefly for the launched app to be registered.
    pub fn expect_launch(&self) -> LaunchGuard {
        self.0.begin_launch();
        LaunchGuard(self.0.clone())
    }

    /// Admits the process family of `pid`, which must be running an
    /// executable inside `bundle`.
    pub fn register(&self, pid: u32, name: &str, bundle: &Path) -> Result<LaunchedApp, String> {
        self.0.register(pid, name, bundle)
    }

    /// Stops admitting a family without a token. Open tunnels finish.
    pub fn forget(&self, id: u64) -> bool {
        self.0.forget(id)
    }

    /// Registered families whose root is still running.
    pub fn launched(&self) -> Vec<LaunchedApp> {
        self.0.launched()
    }
}

pub struct LaunchGuard(Arc<Families>);

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        self.0.end_launch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        processes: Mutex<HashMap<u32, (Identity, PathBuf)>>,
        responsible: Mutex<HashMap<u32, u32>>,
    }

    impl Fake {
        fn spawn(
            &self,
            pid: u32,
            unique_id: u64,
            parent: Option<(u32, u64)>,
            started: u64,
            path: &str,
        ) {
            let (ppid, parent_unique_id) = parent.unwrap_or((1, 1));
            self.processes.lock().unwrap().insert(
                pid,
                (
                    Identity {
                        pid,
                        ppid,
                        unique_id,
                        parent_unique_id,
                        started: (started, 0),
                    },
                    PathBuf::from(path),
                ),
            );
        }

        fn exit(&self, pid: u32) {
            self.processes.lock().unwrap().remove(&pid);
        }
    }

    impl ProcessTable for Arc<Fake> {
        fn identity(&self, pid: u32) -> Option<Identity> {
            self.processes
                .lock()
                .unwrap()
                .get(&pid)
                .map(|(identity, _)| *identity)
        }
        fn responsible(&self, pid: u32) -> Option<u32> {
            let responsible = self.responsible.lock().unwrap();
            Some(responsible.get(&pid).copied().unwrap_or(pid))
        }
        fn executable(&self, pid: u32) -> Option<PathBuf> {
            self.processes
                .lock()
                .unwrap()
                .get(&pid)
                .map(|(_, path)| path.clone())
        }
    }

    fn known(pid: u32, unique_id: u64) -> Attribution {
        Attribution::Known {
            unique_id,
            pid,
            start_time: 0,
            name: "test".into(),
        }
    }

    const APP: &str = "/";

    fn setup() -> (Arc<Fake>, Families<Arc<Fake>>) {
        let fake = Arc::new(Fake::default());
        // The launched app (PID 100), a renderer child (101) and its own child
        // (102), and an XPC helper adopted by launchd but owned by the app (103).
        fake.spawn(
            100,
            1_000,
            None,
            10,
            "/Apps/Browser.app/Contents/MacOS/Browser",
        );
        fake.spawn(
            101,
            1_001,
            Some((100, 1_000)),
            11,
            "/Apps/Browser.app/Contents/Helper",
        );
        fake.spawn(
            102,
            1_002,
            Some((101, 1_001)),
            12,
            "/Apps/Browser.app/Contents/Helper",
        );
        fake.spawn(
            103,
            1_003,
            None,
            13,
            "/Apps/Browser.app/Contents/XPCServices/Net",
        );
        fake.responsible.lock().unwrap().insert(103, 100);
        // An unrelated app.
        fake.spawn(200, 2_000, None, 5, "/Apps/Other.app/Contents/MacOS/Other");
        let families = Families::new(fake.clone());
        (fake, families)
    }

    /// Registers with a bundle path the real filesystem can canonicalize.
    fn register(families: &Families<Arc<Fake>>, pid: u32) -> LaunchedApp {
        families.register(pid, "Browser", Path::new(APP)).unwrap()
    }

    #[test]
    fn launched_process_children_and_helpers_are_members() {
        let (_, families) = setup();
        assert_eq!(
            families.member(&known(100, 1_000)),
            None,
            "nothing registered"
        );
        let app = register(&families, 100);
        for (pid, unique_id) in [(100, 1_000), (101, 1_001), (102, 1_002), (103, 1_003)] {
            assert_eq!(
                families
                    .member(&known(pid, unique_id))
                    .map(|member| member.id),
                Some(app.id),
                "{pid}"
            );
        }
        assert_eq!(
            &*families.member(&known(101, 1_001)).unwrap().app,
            "Browser"
        );
    }

    #[test]
    fn unrelated_unknown_and_reused_pids_are_refused() {
        let (fake, families) = setup();
        register(&families, 100);
        assert_eq!(families.member(&known(200, 2_000)), None);
        assert_eq!(
            families.member(&Attribution::Unknown("No matching socket")),
            None
        );
        // PID 101 exited and an unrelated process took its PID.
        fake.spawn(101, 5_000, None, 50, "/Apps/Other.app/Contents/MacOS/Other");
        assert_eq!(families.member(&known(101, 1_001)), None);
        assert_eq!(families.member(&known(101, 5_000)), None);
        // 102's recorded parent (1_001) no longer matches whatever now runs
        // as PID 101, so the walk stops before it reaches the root.
        assert_eq!(families.member(&known(102, 1_002)), None);
    }

    #[test]
    fn responsible_process_must_predate_the_helper() {
        let (fake, families) = setup();
        // A helper started at 9, before the launched root (started at 10), that
        // names PID 100 as responsible: its real owner exited and the root
        // reused the PID later.
        fake.spawn(104, 1_004, None, 9, "/Apps/Browser.app/Contents/Helper");
        fake.responsible.lock().unwrap().insert(104, 100);
        register(&families, 100);
        assert_eq!(families.member(&known(104, 1_004)), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn executables_outside_the_bundle_are_refused() {
        let fake = Arc::new(Fake::default());
        let bundle = tempfile::tempdir().unwrap();
        let root_path = bundle.path().join("Contents/MacOS/App");
        fake.spawn(100, 1_000, None, 10, root_path.to_str().unwrap());
        // A shell in the app's integrated terminal.
        fake.spawn(101, 1_001, Some((100, 1_000)), 11, "/bin/zsh");
        let families = Families::new(fake.clone());
        assert!(families
            .register(100, "Editor", Path::new("/Applications/Nope.app"))
            .is_err());
        let bundle = std::fs::canonicalize(bundle.path()).unwrap();
        let canonical_root = bundle.join("Contents/MacOS/App");
        fake.spawn(100, 1_000, None, 10, canonical_root.to_str().unwrap());
        let other = tempfile::tempdir().unwrap();
        assert_eq!(
            families.register(100, "Editor", other.path()),
            Err("PID 100 is not running Editor".into())
        );
        families.register(100, "Editor", &bundle).unwrap();
        assert!(families.member(&known(100, 1_000)).is_some());
        assert_eq!(families.member(&known(101, 1_001)), None);
    }

    #[test]
    fn families_drop_after_forget_or_exit() {
        let (fake, families) = setup();
        let app = register(&families, 100);
        assert!(families.contains(app.id));
        assert!(families.forget(app.id));
        assert!(!families.forget(app.id));
        assert_eq!(families.member(&known(101, 1_001)), None);
        let app = register(&families, 100);
        assert_eq!(families.launched().len(), 1);
        fake.exit(100);
        assert!(families.launched().is_empty());
        assert!(!families.contains(app.id));
        assert_eq!(families.member(&known(101, 1_001)), None);
        // Re-registering an exited PID fails.
        assert!(families.register(100, "Browser", Path::new(APP)).is_err());
    }

    #[test]
    fn registering_the_same_process_twice_keeps_one_family() {
        let (_, families) = setup();
        let first = register(&families, 100);
        let second = register(&families, 100);
        assert_eq!(first, second);
        assert_eq!(families.launched().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn unauthorized_requests_wait_only_while_a_launch_is_pending() {
        let (_, families) = setup();
        let families = Arc::new(families);
        let deadline = time::Instant::now() + LAUNCH_GRACE;
        assert!(!families.wait_for_launch(deadline).await);
        families.begin_launch();
        let waiter = tokio::spawn({
            let families = families.clone();
            async move { families.wait_for_launch(deadline).await }
        });
        tokio::task::yield_now().await;
        register(&families, 100);
        assert!(waiter.await.unwrap());
        families.end_launch();
        families.begin_launch();
        let started = time::Instant::now();
        assert!(!families.wait_for_launch(started + LAUNCH_GRACE).await);
        assert_eq!(started.elapsed(), LAUNCH_GRACE);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_processes_resolve_identity_parents_and_paths() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("10")
            .spawn()
            .unwrap();
        let me = System.identity(std::process::id()).unwrap();
        let identity = System.identity(child.id()).unwrap();
        assert_eq!(identity.ppid, std::process::id());
        assert_eq!(identity.parent_unique_id, me.unique_id);
        assert!(identity.started >= me.started);
        assert_eq!(
            System.executable(child.id()).unwrap(),
            Path::new("/bin/sleep")
        );
        assert!(System.responsible(child.id()).is_some());
        let families = Families::default();
        let app = families
            .register(child.id(), "sleep", Path::new("/bin"))
            .unwrap();
        let owner = known(child.id(), identity.unique_id);
        assert_eq!(
            families.member(&owner).map(|member| member.id),
            Some(app.id)
        );
        // The test process is the launched app's parent, not part of its family.
        assert_eq!(
            families.member(&known(std::process::id(), me.unique_id)),
            None
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(families.launched().is_empty());
        assert_eq!(families.member(&owner), None);
        assert_eq!(process_unique_id(child.id()), None);
    }
}
