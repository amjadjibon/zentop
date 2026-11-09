# Alerts and saved views — verification

Verified on macOS on 2026-11-09. All 112 workspace tests, formatting and Clippy
passed. Native UI checks used an isolated `ZENTOP_CONFIG_DIR`; normal preferences
were not changed. The temporary QA app was stopped after testing.

- GUI: created a named view, changed search, loaded the view and verified restored
  search/results. Verified the persisted view JSON and themed popover layout.
- GUI: checked alert thresholds, timing, notification opt-in and recent-history
  layout. Settings tabs now use keyboard-accessible buttons.
- Real alert: memory at/above 50% for 5s produced a timestamped history event.
- macOS desktop delivery: UserNotifications returned OS permission denial for the
  QA bundle. The error appeared beside retained history; no identity spoofing or
  application chooser is used. Actual OS banner display could not be confirmed
  under that permission state. Packaged-app delivery requires OS authorization;
  unbundled GUI/TUI sessions report the limitation and retain in-app alerts.
- Core: deterministic tests exercise sustain, cooldown, recovery, pauses, stale
  frames, gaps, warm-up, missing/invalid data, volume identity and bounded history.
- Storage: round trips, replacement/deletion, case-insensitive names, limits,
  concurrent writers, file permissions and failed/corrupt-file saves are covered.
- TUI: save/load/update/delete, text keys that normally trigger global actions,
  alert edits and narrow-screen selection/rendering are covered.
- Notification results: failures remain visible within a batch and a later
  successful batch clears an earlier error.

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

Linux/Windows notification delivery and native UI were not exercised locally.
