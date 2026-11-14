use super::*;
use crate::monitoring::{ResourceDetails, SystemIdentity, SystemStats, VolumeDetails};
fn frame() -> Frame {
    Frame {
        captured_at: 0,
        sequence: 2,
        processes: vec![],
        gpus: vec![],
        stats: SystemStats {
            temperatures: vec![],
            cpu_usage: vec![95.; 4],
            memory_total: 100,
            memory_used: 95,
            memory_free: 5,
            memory_cached: 0,
            uptime: 0,
            load_avg: [0.; 3],
            network_rx_bytes: None,
            network_tx_bytes: None,
            disk_total_bytes: 100,
            disk_used_bytes: 95,
            disk_free_bytes: 5,
            details: ResourceDetails {
                identity: SystemIdentity {
                    cpu_model: None,
                    physical_cores: None,
                    architecture: String::new(),
                    platform: String::new(),
                    os_version: None,
                    kernel_version: None,
                },
                memory_unallocated: 5,
                swap_total: 0,
                swap_used: 0,
                volumes: vec![VolumeDetails {
                    name: "Data".into(),
                    mount: "/data".into(),
                    file_system: "test".into(),
                    total: 100,
                    available: 5,
                    read_only: false,
                }],
                interfaces: vec![],
            },
        },
    }
}
fn settings() -> Settings {
    let mut s = Settings {
        sustain_secs: 3,
        cooldown_secs: 10,
        ..Default::default()
    };
    s.cpu.enabled = true;
    s
}
fn tick(
    center: &mut Center,
    frame: &mut Frame,
    settings: &Settings,
    start: Instant,
    second: u64,
) -> Vec<Alert> {
    frame.sequence += 1;
    frame.captured_at = second * 1000;
    center.observe(
        frame,
        true,
        Duration::from_secs(1),
        settings,
        start + Duration::from_secs(second),
    )
}
#[test]
fn sustained_threshold_cooldown_and_recovery() {
    let (mut center, mut frame, settings, start) =
        (Center::default(), frame(), settings(), Instant::now());
    for t in 0..3 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 3).len(), 1);
    for t in 4..13 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 13).len(), 1);
    frame.stats.cpu_usage = vec![20.];
    tick(&mut center, &mut frame, &settings, start, 14);
    frame.stats.cpu_usage = vec![95.];
    for t in 15..23 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 23).len(), 1);
}
#[test]
fn warmup_repeated_frames_pauses_gaps_and_missing_values_do_not_advance_hold() {
    let (mut center, mut frame, settings, start) =
        (Center::default(), frame(), settings(), Instant::now());
    frame.sequence = 1;
    assert!(center
        .observe(&frame, true, Duration::from_secs(1), &settings, start)
        .is_empty());
    for t in 1..4 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert!(center
        .observe(
            &frame,
            true,
            Duration::from_secs(1),
            &settings,
            start + Duration::from_secs(4)
        )
        .is_empty());
    center.suspend();
    for t in 4..7 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 7).len(), 1);
    assert!(tick(&mut center, &mut frame, &settings, start, 30).is_empty());
    frame.stats.cpu_usage = vec![f32::NAN];
    assert!(tick(&mut center, &mut frame, &settings, start, 31).is_empty());
    frame.stats.cpu_usage = vec![95.];
    for t in 32..35 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 35).len(), 1);
    // A stale/error snapshot resets pending state even if its sequence repeats.
    assert!(center
        .observe(
            &frame,
            false,
            Duration::from_secs(1),
            &settings,
            start + Duration::from_secs(36)
        )
        .is_empty());
    for t in 36..39 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
}
#[test]
fn rules_are_independent_and_history_is_bounded() {
    let (mut center, mut frame, mut settings, start) =
        (Center::default(), frame(), settings(), Instant::now());
    settings.memory.enabled = true;
    settings.disk_free.enabled = true;
    settings.sustain_secs = 1;
    settings.cooldown_secs = 1;
    for t in 0..100 {
        tick(&mut center, &mut frame, &settings, start, t);
    }
    assert_eq!(center.history.len(), 50);
    assert!(center.history.iter().any(|a| a.kind == Kind::Cpu));
    assert!(center.history.iter().any(|a| a.kind == Kind::Memory));
    assert!(center
        .history
        .iter()
        .any(|a| a.kind == Kind::DiskFree && a.message.contains("/data")));
    settings.cpu.enabled = false;
    settings.memory.enabled = false;
    settings.disk_free.enabled = false;
    assert!(tick(&mut center, &mut frame, &settings, start, 100).is_empty());
}
#[test]
fn disk_hold_tracks_volume_and_ignores_unavailable_or_readonly_data() {
    let (mut center, mut frame, mut settings, start) =
        (Center::default(), frame(), settings(), Instant::now());
    settings.cpu.enabled = false;
    settings.disk_free.enabled = true;
    tick(&mut center, &mut frame, &settings, start, 0);
    tick(&mut center, &mut frame, &settings, start, 1);
    frame.stats.details.volumes[0].mount = "/other".into();
    for t in 2..5 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 5).len(), 1);
    frame.stats.details.volumes[0].read_only = true;
    for t in 6..20 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    frame.stats.details.volumes[0].read_only = false;
    frame.stats.details.volumes[0].total = 0;
    for t in 20..24 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
}
#[test]
fn changed_settings_restart_pending_duration_and_validate_bounds() {
    let (mut center, mut frame, mut settings, start) =
        (Center::default(), frame(), settings(), Instant::now());
    for t in 0..3 {
        tick(&mut center, &mut frame, &settings, start, t);
    }
    settings.cpu.threshold = 94;
    for t in 3..6 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 6).len(), 1);
    settings.cpu.threshold = 0;
    assert!(settings.validate().is_err());
    settings.cpu.threshold = 99;
    settings.sustain_secs = 0;
    assert!(settings.validate().is_err());
    settings.sustain_secs = 1;
    settings.cooldown_secs = 86401;
    assert!(settings.validate().is_err());
}

#[test]
fn missing_memory_and_subthreshold_samples_break_sustain() {
    let (mut center, mut frame, mut settings, start) =
        (Center::default(), frame(), settings(), Instant::now());
    settings.cpu.enabled = false;
    settings.memory.enabled = true;
    for t in 0..3 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    frame.stats.memory_total = 0;
    assert!(tick(&mut center, &mut frame, &settings, start, 3).is_empty());
    frame.stats.memory_total = 100;
    frame.stats.memory_used = 90;
    for t in 4..7 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 7).len(), 1);
    frame.stats.memory_used = 89;
    assert!(tick(&mut center, &mut frame, &settings, start, 8).is_empty());
}

#[cfg(feature = "notifications")]
#[test]
fn delivery_errors_remain_visible_and_later_success_clears_them() {
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    let worker = DesktopNotifications {
        sender: None,
        results: Some(rx),
    };
    tx.send(Err("permission denied".into())).unwrap();
    tx.send(Ok(())).unwrap();
    assert_eq!(worker.poll_result(), Some(Err("permission denied".into())));
    assert_eq!(worker.poll_result(), None);
    tx.send(Ok(())).unwrap();
    assert_eq!(worker.poll_result(), Some(Ok(())));
}

#[test]
fn heat_alerts_use_celsius_and_require_a_fresh_sustained_sensor() {
    let (mut center, mut frame, mut settings, start) =
        (Center::default(), frame(), settings(), Instant::now());
    settings.cpu.enabled = false;
    settings.temperature.enabled = true;
    settings.temperature.threshold = 100;
    assert!(settings.validate().is_ok());
    frame.stats.temperatures = vec![crate::temperature::Sensor {
        id: "cpu".into(),
        name: "CPU package".into(),
        kind: "CPU".into(),
        source: "fixture".into(),
        celsius: Some(105.),
        minimum: Some(105.),
        maximum: Some(105.),
        critical: None,
    }];
    for t in 0..3 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    let fired = tick(&mut center, &mut frame, &settings, start, 3);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].kind, Kind::Temperature);
    assert!(fired[0].message.contains("105.0°C"));
    frame.stats.temperatures[0].celsius = None;
    for t in 4..20 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    frame.stats.temperatures[0].celsius = Some(110.);
    for t in 20..22 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    frame.stats.temperatures[0].id = "different-sensor".into();
    for t in 22..25 {
        assert!(tick(&mut center, &mut frame, &settings, start, t).is_empty());
    }
    assert_eq!(tick(&mut center, &mut frame, &settings, start, 25).len(), 1);
}
