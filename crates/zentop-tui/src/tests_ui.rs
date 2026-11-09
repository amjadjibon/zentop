use crate::{
    app::{App, Effect, Panel},
    ui,
};
use ratatui::{
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    Terminal,
};
use std::{collections::BTreeMap, sync::Arc};
use zentop_core::{
    gpu::GpuDevice,
    monitoring::{
        InterfaceDetails, ProcessInfo, ResourceDetails, SystemIdentity, SystemStats, VolumeDetails,
    },
    process_view::{identity, Column},
    sampling::{Frame, Sample, Snapshot},
};

fn key(app: &mut App, code: KeyCode) -> Effect {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn process(pid: u32, parent: u32, name: &str, cpu: f32, ram: u64) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: parent,
        name: name.into(),
        cpu_usage: cpu,
        memory_usage: ram * 1048576,
        start_time: 1,
        command: format!("/Applications/{name}"),
        status: "Running".into(),
        user: "amjad".into(),
        owner: None,
        gpu_usage: None,
        threads: None,
        environ: vec!["EXAMPLE=value".into()],
        root: "/Applications".into(),
        virtual_memory: 1073741824,
        run_time: 7200,
        disk_usage: (1048576, 2097152),
        session_id: None,
        ports: vec![3000],
    }
}
pub(super) fn fixture() -> App {
    let gib = 1073741824;
    let mut app = App::new(3000, "catppuccin").unwrap();
    let stats = SystemStats {
        cpu_usage: vec![12.5; 12],
        memory_total: 32 * gib,
        memory_used: 16 * gib,
        memory_free: 12 * gib,
        memory_cached: 4 * gib,
        uptime: 172800 + 3660,
        load_avg: [0.5, 0.8, 1.2],
        network_rx_bytes: Some(12582912),
        network_tx_bytes: Some(1048576),
        disk_total_bytes: 480 * gib,
        disk_used_bytes: 300 * gib,
        disk_free_bytes: 180 * gib,
        details: ResourceDetails {
            identity: SystemIdentity {
                cpu_model: Some("Apple M4 Pro".into()),
                physical_cores: Some(12),
                architecture: "arm64".into(),
                platform: "macOS".into(),
                os_version: Some("26.0".into()),
                kernel_version: Some("25.0".into()),
            },
            memory_unallocated: 12 * gib,
            swap_total: 4 * gib,
            swap_used: 0,
            volumes: vec![VolumeDetails {
                name: "Macintosh HD".into(),
                mount: "/".into(),
                file_system: "APFS".into(),
                total: 480 * gib,
                available: 180 * gib,
                read_only: false,
            }],
            interfaces: vec![InterfaceDetails {
                name: "en0".into(),
                received: Some(12582912),
                sent: Some(1048576),
                received_total: gib,
                sent_total: 1000000,
            }],
        },
    };
    let mut helper = process(12, 1, "WebKit helper", 6., 128);
    helper.owner = Some(10);
    let processes = vec![
        process(1, 0, "launchd", 0.1, 32),
        process(10, 0, "Zentop", 12.5, 180),
        process(11, 10, "sampler", 2., 48),
        helper,
        process(20, 0, "node", 8., 512),
    ];
    let frame = Frame {
        captured_at: 123000,
        sequence: 2,
        processes,
        stats,
        gpus: vec![GpuDevice {
            id: "apple".into(),
            name: "Apple M4 Pro".into(),
            vendor: "Apple".into(),
            utilization: Some(28.),
            memory_used: Some(gib / 2),
            memory_total: None,
            memory_kind: "shared".into(),
            temperature: None,
            power_watts: None,
            core_clock_mhz: None,
            cores: Some(20),
            source: "IORegistry".into(),
        }],
    };
    let history = (0..41)
        .map(|i| Sample {
            timestamp: 3000 + i * 3000,
            interval: 3000,
            cpu: if i == 20 {
                None
            } else {
                Some(12.5 + (i % 7) as f64 * 3.)
            },
            memory: Some(50. + (i % 4) as f64),
            received: Some(12582912 + i * 10000),
            sent: Some(1048576),
            gpus: BTreeMap::from([("apple".into(), Some(20. + (i % 9) as f64))]),
        })
        .collect();
    app.snapshot = Some(Snapshot {
        frame: Arc::new(frame),
        history,
        paused: false,
        error: None,
        port_error: None,
    });
    app.rebuild(None);
    app
}
fn render(app: &mut App, width: u16, height: u16) -> TestBackend {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::draw(app, frame)).unwrap();
    terminal.backend().clone()
}
fn text(backend: &TestBackend) -> String {
    backend
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect()
}

#[test]
fn dashboard_has_six_desktop_cards_and_controls() {
    let mut app = fixture();
    let backend = render(&mut app, 160, 42);
    let screen = text(&backend);
    for label in [
        "CPU",
        "Memory",
        "GPU",
        "Storage",
        "System",
        "Network",
        "Search processes",
        "Filters",
        "Columns",
        "Process Name",
        "Catppuccin",
    ] {
        assert!(screen.contains(label), "missing {label}");
    }
    if let Ok(directory) = std::env::var("ZENTOP_TUI_CAPTURE") {
        std::fs::create_dir_all(&directory).unwrap();
        let cells:Vec<_>=backend.buffer().content.iter().map(|c|serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)})).collect();
        std::fs::write(
            std::path::Path::new(&directory).join("dashboard.json"),
            serde_json::to_vec(&serde_json::json!({"width":160,"height":42,"cells":cells}))
                .unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn layouts_and_all_panels_fit_small_and_large_terminals() {
    let mut app = fixture();
    for (width, height) in [
        (160, 42),
        (100, 32),
        (80, 24),
        (55, 20),
        (20, 8),
        (1, 1),
        (0, 0),
    ] {
        for panel in [
            None,
            Some(Panel::Process((10, 1))),
            Some(Panel::Filters),
            Some(Panel::Columns),
            Some(Panel::Settings),
            Some(Panel::Help),
            Some(Panel::Resource(0)),
            Some(Panel::Resource(1)),
            Some(Panel::Resource(2)),
            Some(Panel::Resource(3)),
            Some(Panel::Resource(4)),
            Some(Panel::Resource(5)),
        ] {
            app.panel = panel;
            render(&mut app, width, height);
        }
    }
}

#[test]
fn search_tree_and_family_totals_match_desktop_rules() {
    let mut app = fixture();
    app.tree = true;
    app.search = "sampler".into();
    app.rebuild(None);
    assert_eq!(app.matching, 1);
    assert_eq!(app.rows.len(), 2); // Keep the app parent and expand the matching branch.
    let parent = &app.rows[0];
    assert_eq!(parent.family_count, 3);
    assert_eq!(parent.family_memory, (180 + 48 + 128) * 1048576);
    assert_eq!(parent.family_cpu, 20.5);
}

#[test]
fn search_input_does_not_trigger_hotkeys_and_escape_clears_it() {
    let mut app = fixture();
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('q'));
    assert!(app.searching);
    assert_eq!(app.search, "q");
    assert_eq!(key(&mut app, KeyCode::Esc), Effect::None);
    assert!(app.search.is_empty());
    assert!(!app.searching);
}

#[test]
fn pin_and_selection_survive_live_reordering() {
    let mut app = fixture();
    let id = app.selected_identity().unwrap();
    key(&mut app, KeyCode::Char(' '));
    assert!(!app.pinned.is_empty());
    Arc::make_mut(&mut app.snapshot.as_mut().unwrap().frame)
        .processes
        .reverse();
    app.rebuild(Some(id));
    assert_eq!(app.selected_identity(), Some(id));
}

#[test]
fn filters_columns_and_pause_are_functional() {
    let mut app = fixture();
    key(&mut app, KeyCode::Char('f'));
    key(&mut app, KeyCode::Char(' ')); // CPU > 50, no fixture process matches.
    assert_eq!(app.matching, 0);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.matching, 5);
    key(&mut app, KeyCode::Char('v'));
    key(&mut app, KeyCode::Char(' '));
    assert!(app.columns.contains(&Column::Name));
    key(&mut app, KeyCode::Esc);
    assert_eq!(key(&mut app, KeyCode::Char('p')), Effect::Pause(true));
    assert_eq!(key(&mut app, KeyCode::Char('p')), Effect::Pause(false));
}

#[test]
fn inspector_stays_on_the_same_identity_and_reports_exit() {
    let mut app = fixture();
    let id = app.selected_identity().unwrap();
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panel, Some(Panel::Process(id)));
    Arc::make_mut(&mut app.snapshot.as_mut().unwrap().frame)
        .processes
        .retain(|p| identity(p) != id);
    app.rebuild(Some(id));
    assert!(text(&render(&mut app, 100, 32)).contains("no longer available"));
}

#[test]
fn all_gui_themes_render_and_bad_theme_is_rejected() {
    let mut app = fixture();
    assert_eq!(app.themes.len(), 24);
    for _ in 0..24 {
        key(&mut app, KeyCode::Char('T'));
        render(&mut app, 160, 42);
    }
    assert_eq!(app.theme().name, "catppuccin");
    assert!(App::new(3000, "invalid").is_err());
}

#[test]
fn warmup_and_unavailable_resources_are_explicit() {
    let mut app = fixture();
    let snapshot = app.snapshot.as_mut().unwrap();
    Arc::make_mut(&mut snapshot.frame).sequence = 1;
    Arc::make_mut(&mut snapshot.frame).gpus.clear();
    Arc::make_mut(&mut snapshot.frame).stats.disk_total_bytes = 0;
    assert!(text(&render(&mut app, 160, 42)).contains("Warming up"));
    assert!(text(&render(&mut app, 160, 42)).contains("Unavailable"));
    assert!(text(&render(&mut app, 160, 42)).contains("Capacity not reported"));
}

#[test]
fn waiting_layout_handles_empty_and_narrow_terminals() {
    let mut app = App::new(3000, "catppuccin").unwrap();
    for (w, h) in [(160, 42), (80, 24), (1, 1), (0, 0)] {
        render(&mut app, w, h);
    }
}

#[test]
fn saved_views_capture_text_keys_restore_settings_and_delete() {
    let mut app = fixture();
    let directory = std::env::temp_dir().join(format!("zentop-tui-views-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    app.features =
        zentop_core::monitoring_preferences::Store::open_at(directory.join("monitoring.json")).0;
    app.search = "node".into();
    app.filters.memory.enabled = true;
    app.sort.column = Column::Memory;
    app.sort.ascending = true;
    app.tree = true;
    app.columns = vec![Column::Name, Column::Memory];
    app.pinned.insert("/Applications/node".into());
    let expected = app.capture_view("qTpb".into());
    key(&mut app, KeyCode::Char('b'));
    assert_eq!(app.panel, Some(Panel::Views));
    key(&mut app, KeyCode::Char('n'));
    let theme = app.theme_index;
    for ch in "qTpb".chars() {
        assert_eq!(key(&mut app, KeyCode::Char(ch)), Effect::None);
    }
    assert_eq!(app.theme_index, theme);
    assert!(!app.paused);
    key(&mut app, KeyCode::Enter);
    assert!(app.feature_error.is_none());
    assert_eq!(app.features.config.views, vec![expected.clone()]);
    app.search.clear();
    app.tree = false;
    app.filters = Default::default();
    app.pinned.clear();
    key(&mut app, KeyCode::Enter);
    assert!(app.panel.is_none());
    assert_eq!(app.capture_view("qTpb".into()), expected);
    key(&mut app, KeyCode::Char('b'));
    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.features.config.views.len(), 1);
    key(&mut app, KeyCode::Delete);
    assert!(app.features.config.views.is_empty());
    assert!(app.feature_error.is_none());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn alert_sheet_persists_rules_and_small_panels_render() {
    let mut app = fixture();
    let directory = std::env::temp_dir().join(format!("zentop-tui-alerts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    app.features =
        zentop_core::monitoring_preferences::Store::open_at(directory.join("monitoring.json")).0;
    key(&mut app, KeyCode::Char('a'));
    assert_eq!(app.panel, Some(Panel::Alerts));
    key(&mut app, KeyCode::Char(' '));
    assert!(app.features.config.alerts.cpu.enabled);
    key(&mut app, KeyCode::Left);
    assert_eq!(app.features.config.alerts.cpu.threshold, 89);
    for _ in 0..3 {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Right);
    assert_eq!(app.features.config.alerts.sustain_secs, 35);
    let saved =
        zentop_core::monitoring_preferences::Store::open_at(directory.join("monitoring.json")).0;
    assert_eq!(saved.config.alerts, app.features.config.alerts);
    for panel in [Panel::Alerts, Panel::Views] {
        app.panel = Some(panel);
        app.panel_index = 0;
        for (width, height) in [(20, 8), (45, 16), (80, 24), (150, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| ui::draw(&mut app, frame)).unwrap();
        }
    }
    app.panel = Some(Panel::Views);
    key(&mut app, KeyCode::Char('n'));
    key(&mut app, KeyCode::Enter);
    assert!(app.feature_error.is_some());
    assert!(app.view_name.is_some());
    key(&mut app, KeyCode::Esc);
    assert!(app.view_name.is_none());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn wrapped_saved_view_selection_remains_visible_in_a_narrow_terminal() {
    let mut app = fixture();
    app.features.config.views = (0..32)
        .map(|i| {
            app.capture_view(format!(
                "Preset{i:02} with a long descriptive monitoring name"
            ))
        })
        .collect();
    app.panel = Some(Panel::Views);
    for _ in 0..31 {
        key(&mut app, KeyCode::Down);
    }
    let backend = render(&mut app, 40, 16);
    assert_eq!(app.panel_index, 31);
    assert!(text(&backend).contains("Preset31"));
}
