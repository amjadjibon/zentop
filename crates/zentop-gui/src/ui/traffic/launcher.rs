//! Open with Zentop: finds Chromium browsers and Electron apps, builds their
//! launch arguments, manages temporary browser profiles, and launches and
//! quits apps through AppKit. See "Phase 3D" in the traffic design.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Chromium browsers known to honour `--proxy-server` and `--user-data-dir`.
/// Arc is left out until its handling of the switch is verified.
const BROWSERS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.dev",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "com.brave.Browser",
    "com.brave.Browser.beta",
    "com.brave.Browser.nightly",
    "com.microsoft.edgemac",
    "com.microsoft.edgemac.Beta",
    "com.microsoft.edgemac.Dev",
    "com.microsoft.edgemac.Canary",
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
    "com.operasoftware.OperaGX",
];

/// Zentop's own bundle never appears in the list.
const SELF: &str = "com.zentop.app";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AppKind {
    /// A Chromium browser: launched as a separate instance with a temporary
    /// profile.
    Browser,
    /// An Electron app: launched with its normal data, one instance at a time.
    Electron,
    /// Anything else the user chose; it may ignore the proxy switch.
    Other,
}

impl AppKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Browser => "Browser",
            Self::Electron => "Electron",
            Self::Other => "App",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct InstalledApp {
    pub name: String,
    pub bundle_id: String,
    pub path: PathBuf,
    pub kind: AppKind,
    pub icon: Option<crate::native_icons::Icon>,
}

impl PartialEq for InstalledApp {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.bundle_id == other.bundle_id && self.kind == other.kind
    }
}

/// The folders searched for apps: `/Applications` and `~/Applications`.
pub(super) fn search_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    roots
}

/// Lists launchable apps under `roots`, including one level of subfolders.
/// Browsers come first, then Electron apps, each by name.
pub(super) fn discover(roots: &[PathBuf]) -> Vec<InstalledApp> {
    let mut apps = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let candidates = if is_bundle(&path) {
                vec![path]
            } else if path.is_dir() && !path.is_symlink() {
                std::fs::read_dir(&path)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| is_bundle(path))
                    .collect()
            } else {
                Vec::new()
            };
            for candidate in candidates {
                let Some(app) = inspect(&candidate) else {
                    continue;
                };
                if app.kind != AppKind::Other
                    && app.bundle_id != SELF
                    && seen.insert(app.path.clone())
                {
                    apps.push(app);
                }
            }
        }
    }
    apps.sort_by(|a, b| {
        (a.kind, a.name.to_lowercase(), &a.path).cmp(&(b.kind, b.name.to_lowercase(), &b.path))
    });
    apps
}

fn is_bundle(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        && path.is_dir()
}

/// Reads one `.app` bundle, classifying it as a browser, an Electron app or
/// another app. Returns `None` for anything that isn't an app bundle.
pub(super) fn inspect(path: &Path) -> Option<InstalledApp> {
    if !is_bundle(path) {
        return None;
    }
    let info = info_plist(&path.join("Contents/Info.plist"))?;
    let bundle_id = info.bundle_id?;
    let kind = if BROWSERS.contains(&bundle_id.as_str()) {
        AppKind::Browser
    } else if path
        .join("Contents/Frameworks/Electron Framework.framework")
        .is_dir()
    {
        AppKind::Electron
    } else {
        AppKind::Other
    };
    let name = info
        .name
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })?;
    Some(InstalledApp {
        name,
        bundle_id,
        path: path.to_owned(),
        kind,
        icon: None,
    })
}

struct Info {
    bundle_id: Option<String>,
    name: Option<String>,
}

/// Reads an XML or binary property list.
fn info_plist(path: &Path) -> Option<Info> {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::propertylist::{
        create_with_data, kCFPropertyListImmutable, CFPropertyList,
    };
    use core_foundation::string::CFString;

    let bytes = std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() <= 4 << 20)?;
    let (plist, _) =
        create_with_data(CFData::from_buffer(&bytes), kCFPropertyListImmutable).ok()?;
    // `create_with_data` returns an owned reference.
    let plist = unsafe { CFPropertyList::wrap_under_create_rule(plist) };
    let dictionary: CFDictionary = plist.downcast_into()?;
    let text = |key: &str| {
        let key = CFString::new(key);
        let value = dictionary.find(key.as_concrete_TypeRef().cast())?;
        // The dictionary keeps the value alive while it is retained here.
        let value = unsafe { CFType::wrap_under_get_rule(*value) };
        value.downcast::<CFString>().map(|text| text.to_string())
    };
    Some(Info {
        bundle_id: text("CFBundleIdentifier"),
        name: text("CFBundleDisplayName").or_else(|| text("CFBundleName")),
    })
}

/// The arguments an app is launched with. The proxy URL never carries the
/// session token: launched apps are admitted by process family instead.
pub(super) fn launch_arguments(
    kind: AppKind,
    proxy: SocketAddr,
    profile: Option<&Path>,
    spki: Option<&str>,
) -> Vec<String> {
    let mut arguments = vec![
        format!("--proxy-server=http://{proxy}"),
        // QUIC is UDP and never proxied.
        "--disable-quic".to_owned(),
    ];
    if kind == AppKind::Browser {
        if let Some(profile) = profile {
            arguments.push(format!("--user-data-dir={}", profile.display()));
        }
        arguments.push("--no-first-run".to_owned());
        arguments.push("--no-default-browser-check".to_owned());
    }
    if let Some(spki) = spki {
        arguments.push(format!("--ignore-certificate-errors-spki-list={spki}"));
    }
    arguments
}

/// Where temporary browser profiles live: one folder per launch, named after
/// the Zentop process that created it.
pub(super) fn profiles_root() -> PathBuf {
    std::env::temp_dir().join("zentop-launched-profiles")
}

pub(super) fn new_profile(root: &Path) -> std::io::Result<PathBuf> {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    let mut attempt = 0;
    loop {
        // Each RandomState is randomly keyed, so this is a fresh random value.
        let suffix = RandomState::new().hash_one(attempt);
        let path = root.join(format!("{}-{suffix:016x}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => {
                attempt += 1;
            }
            result => return result.map(|()| path),
        }
    }
}

/// Profiles left behind by a Zentop that is gone, whose browser is gone too.
/// `alive` says whether a PID is running. A profile whose owner is unknown
/// is kept.
pub(super) fn stale_profiles(root: &Path, alive: impl Fn(u32) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && !path.is_symlink())
        .filter(|path| {
            let owner = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split_once('-'))
                .and_then(|(pid, _)| pid.parse::<u32>().ok());
            // Chromium's `SingletonLock` links to "<host>-<pid>".
            let browser = std::fs::read_link(path.join("SingletonLock"))
                .ok()
                .and_then(|target| {
                    target
                        .to_str()
                        .and_then(|target| target.rsplit_once('-'))
                        .and_then(|(_, pid)| pid.parse::<u32>().ok())
                });
            owner.is_some_and(|owner| !alive(owner)) && browser.is_none_or(|pid| !alive(pid))
        })
        .collect()
}

/// Deletes a profile Zentop created. Paths outside the profiles folder are
/// never touched.
pub(super) fn remove_profile(root: &Path, profile: &Path) {
    if profile.parent() == Some(root) {
        let _ = std::fs::remove_dir_all(profile);
    }
}

pub(super) fn pid_alive(pid: u32) -> bool {
    zentop_proxy::process_unique_id(pid).is_some()
}

/// Launches `app` as a new instance with `arguments`, returning its PID.
pub(super) fn launch(app: &Path, arguments: &[String]) -> Result<u32, String> {
    native::launch(app, arguments)
}

/// PIDs of running instances of a bundle.
pub(super) fn running(bundle_id: &str) -> Vec<u32> {
    native::running(bundle_id)
}

/// Asks an app to quit, as choosing Quit from its menu would. It may ask to
/// save work first. Never sends a signal.
pub(super) fn quit(pid: u32) -> bool {
    native::terminate(pid)
}

mod native {
    use block2::RcBlock;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::{NSArray, NSError, NSString, NSURL};
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    pub(super) fn launch(app: &Path, arguments: &[String]) -> Result<u32, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        autoreleasepool(|_| {
            let path = NSString::from_str(&app.to_string_lossy());
            let url = NSURL::fileURLWithPath_isDirectory(&path, true);
            let arguments: Vec<Retained<NSString>> = arguments
                .iter()
                .map(|argument| NSString::from_str(argument))
                .collect();
            let arguments = NSArray::from_retained_slice(&arguments);
            let handler = RcBlock::new(move |running: *mut AnyObject, error: *mut NSError| {
                let result = if running.is_null() {
                    // A non-null error describes why the launch failed.
                    Err(unsafe { error.as_ref() }.map_or_else(
                        || "The app could not be opened".to_owned(),
                        |error| error.localizedDescription().to_string(),
                    ))
                } else {
                    // `running` is a live NSRunningApplication for this call.
                    let pid: i32 = unsafe { msg_send![running, processIdentifier] };
                    u32::try_from(pid).map_err(|_| "The app exited while opening".to_owned())
                };
                let _ = sender.try_send(result);
            });
            // NSWorkspace and its open configuration are thread-safe; the
            // completion handler runs on a private queue.
            unsafe {
                let configuration: Retained<AnyObject> =
                    msg_send![class!(NSWorkspaceOpenConfiguration), configuration];
                let _: () = msg_send![&*configuration, setArguments: &*arguments];
                let _: () = msg_send![&*configuration, setCreatesNewApplicationInstance: true];
                let _: () = msg_send![&*configuration, setAddsToRecentItems: false];
                let workspace: Retained<AnyObject> =
                    msg_send![class!(NSWorkspace), sharedWorkspace];
                let _: () = msg_send![
                    &*workspace,
                    openApplicationAtURL: &*url,
                    configuration: &*configuration,
                    completionHandler: &*handler
                ];
            }
        });
        receiver
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| "Timed out waiting for the app to open".to_owned())?
    }

    pub(super) fn running(bundle_id: &str) -> Vec<u32> {
        autoreleasepool(|_| {
            let bundle_id = NSString::from_str(bundle_id);
            // Returns an autoreleased array of NSRunningApplication.
            let apps: Retained<NSArray<AnyObject>> = unsafe {
                msg_send![
                    class!(NSRunningApplication),
                    runningApplicationsWithBundleIdentifier: &*bundle_id
                ]
            };
            apps.iter()
                .filter(|app| {
                    let terminated: Bool = unsafe { msg_send![&**app, isTerminated] };
                    !terminated.as_bool()
                })
                .filter_map(|app| {
                    let pid: i32 = unsafe { msg_send![&*app, processIdentifier] };
                    u32::try_from(pid).ok().filter(|&pid| pid > 0)
                })
                .collect()
        })
    }

    pub(super) fn terminate(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        autoreleasepool(|_| {
            let app: Option<Retained<AnyObject>> = unsafe {
                msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid]
            };
            app.is_some_and(|app| {
                let sent: Bool = unsafe { msg_send![&*app, terminate] };
                sent.as_bool()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(root: &Path, name: &str, id: &str, electron: bool) -> PathBuf {
        let path = root.join(format!("{name}.app"));
        std::fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
        std::fs::write(
            path.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundleExecutable</key><string>{name}</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        if electron {
            std::fs::create_dir_all(path.join("Contents/Frameworks/Electron Framework.framework"))
                .unwrap();
        }
        path
    }

    #[test]
    fn discovers_browsers_by_id_and_electron_apps_by_framework() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        bundle(root.path(), "Google Chrome", "com.google.Chrome", false);
        bundle(root.path(), "Slack", "com.tinyspeck.slackmacgap", true);
        bundle(root.path(), "Notes", "com.apple.Notes", false);
        bundle(root.path(), "Zentop", SELF, true);
        // One level of subfolders is searched, deeper ones are not.
        bundle(
            &root.path().join("Vendor"),
            "Brave Browser",
            "com.brave.Browser",
            false,
        );
        bundle(
            &root.path().join("a/b"),
            "Deep",
            "com.google.Chrome.canary",
            false,
        );
        bundle(
            home.path(),
            "Visual Studio Code",
            "com.microsoft.VSCode",
            true,
        );
        // A folder named like an app but without a plist.
        std::fs::create_dir_all(root.path().join("Broken.app/Contents")).unwrap();
        let apps = discover(&[root.path().to_owned(), home.path().to_owned()]);
        let names: Vec<_> = apps
            .iter()
            .map(|app| (app.name.as_str(), app.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("Brave Browser", AppKind::Browser),
                ("Google Chrome", AppKind::Browser),
                ("Slack", AppKind::Electron),
                ("Visual Studio Code", AppKind::Electron),
            ]
        );
        assert_eq!(apps[1].bundle_id, "com.google.Chrome");
        assert!(apps[1].path.ends_with("Google Chrome.app"));
    }

    #[test]
    fn inspects_a_chosen_bundle_of_any_kind() {
        let root = tempfile::tempdir().unwrap();
        let notes = bundle(root.path(), "Notes", "com.apple.Notes", false);
        assert_eq!(inspect(&notes).unwrap().kind, AppKind::Other);
        assert!(inspect(&root.path().join("missing.app")).is_none());
        assert!(inspect(root.path()).is_none());
    }

    #[test]
    fn reads_binary_property_lists() {
        // Every installed copy of Safari ships a binary Info.plist.
        let Some(safari) = inspect(Path::new("/System/Applications/Safari.app"))
            .or_else(|| inspect(Path::new("/Applications/Safari.app")))
        else {
            return;
        };
        assert_eq!(safari.bundle_id, "com.apple.Safari");
        assert_eq!(safari.kind, AppKind::Other);
    }

    #[test]
    fn browser_arguments_use_a_temporary_profile_and_no_token() {
        let proxy: SocketAddr = "127.0.0.1:54139".parse().unwrap();
        let profile = Path::new("/tmp/zentop-launched-profiles/1-ab");
        assert_eq!(
            launch_arguments(AppKind::Browser, proxy, Some(profile), None),
            [
                "--proxy-server=http://127.0.0.1:54139",
                "--disable-quic",
                "--user-data-dir=/tmp/zentop-launched-profiles/1-ab",
                "--no-first-run",
                "--no-default-browser-check",
            ]
        );
        let with_spki = launch_arguments(AppKind::Browser, proxy, Some(profile), Some("abc="));
        assert_eq!(
            with_spki.last().unwrap(),
            "--ignore-certificate-errors-spki-list=abc="
        );
        assert!(
            launch_arguments(AppKind::Browser, proxy, Some(profile), None)
                .iter()
                .all(|argument| !argument.contains("spki") && !argument.contains('@'))
        );
    }

    #[test]
    fn electron_arguments_keep_the_apps_own_data() {
        let proxy: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert_eq!(
            launch_arguments(AppKind::Electron, proxy, None, None),
            ["--proxy-server=http://127.0.0.1:8080", "--disable-quic"]
        );
        assert_eq!(
            launch_arguments(AppKind::Electron, proxy, None, Some("k=")),
            [
                "--proxy-server=http://127.0.0.1:8080",
                "--disable-quic",
                "--ignore-certificate-errors-spki-list=k=",
            ]
        );
        // A profile is only ever used for browsers.
        let other = launch_arguments(AppKind::Other, proxy, Some(Path::new("/tmp/p")), None);
        assert!(other
            .iter()
            .all(|argument| !argument.starts_with("--user-data-dir")));
    }

    #[test]
    fn stale_profiles_need_both_owner_and_browser_gone() {
        let root = tempfile::tempdir().unwrap();
        let make = |name: &str, browser: Option<u32>| {
            let path = root.path().join(name);
            std::fs::create_dir(&path).unwrap();
            if let Some(pid) = browser {
                std::os::unix::fs::symlink(format!("host.local-{pid}"), path.join("SingletonLock"))
                    .unwrap();
            }
            path
        };
        let gone = make("100-aa", None);
        let browser_running = make("101-bb", Some(500));
        let browser_gone = make("102-cc", Some(501));
        let owner_running = make("200-dd", None);
        let _unknown = make("not-a-pid", None);
        let alive = |pid: u32| [200, 500].contains(&pid);
        let mut stale = stale_profiles(root.path(), alive);
        stale.sort();
        assert_eq!(stale, [gone.clone(), browser_gone.clone()]);
        assert!(!stale.contains(&browser_running) && !stale.contains(&owner_running));
        remove_profile(root.path(), &gone);
        assert!(!gone.exists());
        // Never outside the profiles folder.
        let outside = tempfile::tempdir().unwrap();
        remove_profile(root.path(), outside.path());
        assert!(outside.path().exists());
    }

    #[test]
    fn new_profiles_are_private_and_named_for_this_process() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let profiles = root.path().join("profiles");
        let first = new_profile(&profiles).unwrap();
        let second = new_profile(&profiles).unwrap();
        assert_ne!(first, second);
        assert!(first
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(&format!("{}-", std::process::id())));
        let mode = std::fs::metadata(&first).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(stale_profiles(&profiles, |pid| pid == std::process::id()).is_empty());
    }
}
