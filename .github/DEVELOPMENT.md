# Developing and releasing Zentop

Use Rust 1.88 or newer. Zentop is a Cargo workspace:

- `crates/zentop-core`: system/process/GPU monitoring, sampling and history; no GPUI dependency.
- `crates/zentop-gui`: the GPUI Kit desktop app, icons and preferences. All windows
  observe `src/ui/monitor.rs` within this crate.
- `crates/zentop-tui`: Ratatui terminal monitor; Clap arguments and a background sampler.
- `crates/zentop-cli`: Clap system/process reports with text and JSON output.

The GUI is the default member, preserving `cargo run` at the repository root.
Both terminal frontends use `zentop-core` without pulling in desktop dependencies. Core tests can run independently
with `cargo test -p zentop-core`, without desktop build dependencies.

Compose gpui-component controls with the theme roles in
`crates/zentop-gui/src/ui/theme.rs` before adding custom widgets. Shared assets
stay in `assets/`, and release tooling is plain Bash in `scripts/`.

On Linux, install the GPUI build dependencies first, for example on Ubuntu:
`libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan-dev libgtk-3-dev libayatana-appindicator3-dev`
(GTK and AppIndicator power the system-tray icon).

### Setup

```bash
# Run in development mode
cargo run

# Run tests
cargo test --workspace

# Build installers (dmg/app, nsis, deb, AppImage) into target/packager
cargo install cargo-packager --locked
cargo build -p zentop-gui --release
cargo packager -p zentop-gui --release

# macOS DMG into artifacts/ (ad-hoc signed; pass aarch64 or x86_64 to pick)
scripts/build-dmg.sh

# RPM (Linux)
cargo install cargo-generate-rpm --locked
(cd crates/zentop-gui && cargo generate-rpm)
```

Themes live in `assets/themes.json` as resolved semantic roles. `cargo test --workspace`
checks every theme for readable contrast (WCAG AA for text and labels, 3:1 for
controls and chart colors) on each surface it is used on.

### Terminal frontends

```bash
cargo run -p zentop-cli -- system
cargo run -p zentop-cli -- processes --sort cpu --limit 20
cargo run -p zentop-cli -- processes --sort memory --json
cargo run -p zentop-tui -- --interval 3000
```

Both apps use Clap for help, version and argument validation. CLI defaults to
`system` and waits 1.5 seconds between observations so CPU/network rates have a
baseline. `--json` works before or after a subcommand. Process reports export
PID, name, CPU, RAM and status; they do not dump environment variables.

The TUI uses Ratatui with its Crossterm backend and requires an interactive
terminal. It mirrors the six desktop resource cards, List/Tree process views,
search, filters, pins, columns and inspection sheets. Terminal shortcuts are in
[the user guide](../docs/usage.md#terminal-apps). `--theme` selects any palette
from `assets/themes.json`; press `T` to cycle. Choices last for the terminal
session and do not write GUI preferences.

Shared process presentation rules live in `zentop-core/src/process_view.rs`.
The GUI re-exports them, so search syntax, filter comparisons, family ownership
and CPU/RAM aggregation are identical. Sampling runs on a worker thread with
pause, interval and on-demand GPU/port commands and a bounded snapshot queue.
Terminal state is restored on exit, errors and panics through Ratatui's hook.
CPU/network rates use a warmup observation; missing chart samples remain gaps.

TUI renderer tests cover all palettes, narrow terminals and sheets. For a
deterministic cell/color capture, set `ZENTOP_TUI_CAPTURE=/tmp/zentop-capture`
and run `cargo test -p zentop-tui dashboard_has_six_desktop_cards_and_controls`.

### Tests and releases

CI covers macOS only. **Test** (`.github/workflows/test.yml`) runs on pull
requests and pushes to `main`: formatting, the release-script tests, the version
check, Clippy and `cargo test --workspace`.

**Release** (`.github/workflows/release.yml`) runs the tests, then builds the
Apple silicon DMG. Other platforms aren't built or released by CI. To release:

1. Update `[workspace.package].version` in the root `Cargo.toml`, run `cargo check` to refresh `Cargo.lock`,
   then run `scripts/release.sh check-version` and commit.
2. Push a matching tag, for example `git tag v1.4.0 && git push origin v1.4.0`.
   The DMG, `SHA256SUMS` and `release-manifest.json` are attached to a
   draft GitHub release. A published release is never overwritten.
3. Install and launch the DMG, review the notes, then publish the draft.

Running **Release** by hand on a branch builds a preview DMG, downloadable from
the run's artifacts, without making a release.

macOS packages are ad-hoc signed unless the repository has the secrets
`MACOS_CERTIFICATE` (a base64-encoded Developer ID `.p12`),
`MACOS_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD` (an app-specific
password) and `APPLE_TEAM_ID`, plus the variable `APPLE_SIGNING_IDENTITY`. With
them, the app is signed and notarized.

To build locally, use `scripts/build-dmg.sh` on macOS, or
`cargo packager -p zentop-gui --release` on Windows and Linux (plus `(cd crates/zentop-gui && cargo generate-rpm)` and
`scripts/release.sh check-appimage target/packager` on Linux, which makes sure the
AppImage doesn't bundle the host's Wayland libraries; see issue #203).

### Code Formatting and Checks

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/release-test.sh
```

Before submitting a pull request, make sure the code is formatted, the tests pass,
and the app builds and runs on your platform.
