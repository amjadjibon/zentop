use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "zentop-monitoring-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn path(&self) -> PathBuf {
        self.0.join("monitoring.json")
    }
    fn store(&self) -> Store {
        let (store, error) = Store::open_at(self.path());
        assert!(error.is_none());
        store
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn view(name: &str) -> SavedView {
    let mut filters = Filters::default();
    filters.memory.enabled = true;
    SavedView {
        name: name.into(),
        search: "node :3000".into(),
        filters,
        sort: Sort {
            column: Column::Memory,
            ascending: true,
        },
        columns: vec![Column::Name, Column::Cpu, Column::Memory],
        tree: true,
        pinned: vec!["/bin/node server.js".into()],
    }
}
#[test]
fn independent_frontends_merge_and_restore_all_view_fields() {
    let fixture = Fixture::new();
    let mut gui = fixture.store();
    let mut tui = fixture.store();
    gui.save_view(view(" Development ")).unwrap();
    let mut settings = alerts::Settings::default();
    settings.cpu.enabled = true;
    tui.set_alerts(settings.clone()).unwrap();
    tui.save_view(view("Gaming")).unwrap();
    gui.reload().unwrap();
    assert_eq!(gui.config.views, vec![view("Development"), view("Gaming")]);
    assert_eq!(gui.config.alerts, settings);
    let mut changed = view("development");
    changed.search = "rustc".into();
    gui.save_view(changed.clone()).unwrap();
    assert_eq!(fixture.store().config.views, vec![changed, view("Gaming")]);
    tui.delete_view("development").unwrap();
    assert_eq!(fixture.store().config.views, vec![view("Gaming")]);
    assert_eq!(fixture.store().config.alerts, settings);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn invalid_data_and_failed_writes_leave_existing_state_intact() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    store.save_view(view("valid")).unwrap();
    let original = fs::read(fixture.path()).unwrap();
    let mut invalid = view("");
    assert!(store.save_view(invalid.clone()).is_err());
    invalid.name = "bad".into();
    invalid.filters.cpu.value = f64::NAN;
    assert!(store.save_view(invalid).is_err());
    let mut invalid = view("bad");
    invalid.columns = vec![Column::Cpu];
    assert!(store.save_view(invalid).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    fs::write(fixture.path(), b"{invalid").unwrap();
    assert!(store.delete_view("valid").is_err());
    assert_eq!(store.config.views, vec![view("valid")]);
    assert_eq!(fs::read(fixture.path()).unwrap(), b"{invalid");
    assert!(Store::open_at(fixture.path()).1.is_some());
    fs::write(fixture.path(), &original).unwrap();
    fs::create_dir(fixture.path().with_extension("json.tmp")).unwrap();
    assert!(store.delete_view("valid").is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert_eq!(store.config.views, vec![view("valid")]);
}
#[test]
fn limits_and_partial_configuration_are_validated() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    for i in 0..32 {
        store.save_view(view(&format!("view-{i}"))).unwrap();
    }
    assert!(store.save_view(view("one-too-many")).is_err());
    assert_eq!(store.config.views.len(), 32);
    store.save_view(view("VIEW-0")).unwrap();
    assert_eq!(store.config.views.len(), 32);
    fs::write(fixture.path(), b"{}").unwrap();
    assert_eq!(fixture.store().config, Configuration::default());
    fs::write(fixture.path(), br#"{"version":2}"#).unwrap();
    assert!(Store::open_at(fixture.path()).1.is_some());
}
#[test]
fn concurrent_writers_preserve_independent_views() {
    let fixture = Fixture::new();
    std::thread::scope(|scope| {
        for i in 0..8 {
            let path = fixture.path();
            scope.spawn(move || {
                let (mut store, error) = Store::open_at(path);
                assert!(error.is_none());
                store.save_view(view(&format!("writer-{i}"))).unwrap();
            });
        }
    });
    assert_eq!(fixture.store().config.views.len(), 8);
}
