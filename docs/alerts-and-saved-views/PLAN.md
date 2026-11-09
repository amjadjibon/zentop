---
goal: Add resource alerts and saved monitoring views
version: 1.0
date_created: 2026-11-09
last_updated: 2026-11-09
owner: Zentop
status: 'In progress'
tags: [feature]
---
# Resource alerts and saved monitoring views

![Status: In progress](https://img.shields.io/badge/status-In%20progress-yellow)

Implement the shared design in DESIGN.md and expose both features in GUI and TUI.

## 1. Requirements & Constraints
- REQ-001: Configurable sustained CPU, memory and low-space alerts with cooldowns.
- REQ-002: Save, restore, replace and delete named monitoring views across both frontends.
- SEC-001: Local-only bounded configuration/history; no environment values in saved views.
- CON-001: Preserve pause semantics, compatible existing preferences and themed controls.

## 2. Implementation Steps
### Phase 1: Shared monitoring features
**Goal**: Deliver both features through one tested core and consistent frontend adapters.
- [x] TASK-001: Implement alert engine, notification worker, validated shared storage and saved view model.
- [x] TASK-002: Add GUI alert settings/history and toolbar view management.
- [x] TASK-003: Add TUI alert and saved-view sheets with captured name entry.
- [x] TASK-004: Test timing, invalid data, persistence and restore behavior; document controls.
- [ ] TASK-005: Run workspace tests, formatting and Clippy; review and commit.

**Completion criteria**: Both frontends support save/load/delete and alert configuration;
core tests verify timing and bounded state; workspace tests, formatting and Clippy pass.
**git commit**: `git add -u && git commit -m "feat: add resource alerts and saved monitoring views"`
**Agent Prompt**:
```
Implement alerts-and-saved-views on feature/alerts-and-saved-views, based on main plus
its local memory/theme commits. Follow DESIGN.md. Own core alerts/storage, GUI
monitor/settings/main_window/table and TUI app/ui integration. Verify thresholds,
cooldowns, pauses/errors and view restoration with tests. Run workspace tests,
formatting and Clippy. Commit without Co-authored-by; do not push or open PRs.
```

## 3. Testing
- [x] TEST-001: Deterministic alert duration/cooldown/gap/missing-value tests.
- [x] TEST-002: Persistence round trips, validation, merged independent saves and failed writes.
- [x] TEST-003: TUI save/load/delete/name entry, alerts controls and small-screen rendering.
- [ ] TEST-004: GUI compile and native visual check; all workspace checks.

## 4. Risks & Assumptions
- RISK-001: OS notification delivery can fail — retain history and show errors.
- RISK-002: Concurrent frontend writes — file lock, reload and atomic replacement.
- ASSUMPTION-001: Alert rules are opt-in, with 30s sustain and 5min cooldown defaults.

## Implementation decisions from native QA
- macOS uses the current UserNotifications API with bounded authorization/submission
  waits. This replaces legacy bundle-identity overrides that produced an application
  chooser during QA. Unbundled macOS executables retain in-app alerts/history.
- `ZENTOP_CONFIG_DIR` supports an isolated QA/portable profile without touching the
  user's normal settings. No push or PR is part of this local implementation.
