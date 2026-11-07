# Developing and releasing Zentop

Use Rust 1.88 or newer. The desktop app is a single GPUI Kit crate: system
sampling in `src/`, windows in `src/ui/` (main window, Settings, About and the
menu-bar overview, all observing the shared model in `src/ui/monitor.rs`).
Compose gpui-component controls with the theme roles in `src/ui/theme.rs` before
adding custom widgets. Release tooling is plain Bash in `scripts/`.

On Linux, install the GPUI build dependencies first, for example on Ubuntu:
`libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan-dev libgtk-3-dev libayatana-appindicator3-dev`
(GTK and AppIndicator power the system-tray icon).

### Setup

```bash
# Run in development mode
cargo run

# Run tests
cargo test

# Build installers (dmg/app, nsis, deb, AppImage) into target/packager
cargo install cargo-packager --locked
cargo build --release
cargo packager --release

# macOS DMG into artifacts/ (ad-hoc signed; pass aarch64 or x86_64 to pick)
scripts/build-dmg.sh

# RPM (Linux)
cargo install cargo-generate-rpm --locked
cargo generate-rpm
```

Themes live in `assets/themes.json` as resolved semantic roles. `cargo test`
checks every theme for readable contrast (WCAG AA for text and labels, 3:1 for
controls and chart colors) on each surface it is used on.

### Releases

There is no CI; build and check releases locally on each platform.

1. Update the version in `Cargo.toml` and run `cargo check` to refresh `Cargo.lock`.
   Run `scripts/release.sh check-version` and commit the changes.
2. Before packaging, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test` and `scripts/release-test.sh`.
3. Build packages on each platform: `scripts/build-dmg.sh` on macOS (ad-hoc signed;
   export the `APPLE_*` signing variables to sign and notarize), or
   `cargo packager --release` on Windows and Linux. On Linux, also run
   `cargo generate-rpm`, then `scripts/release.sh check-appimage target/packager`
   to make sure the AppImage doesn't bundle the host's Wayland libraries (issue #203).
4. Install and launch each package on its operating system before publishing a
   GitHub release.

### Code Formatting and Checks

```bash
cargo fmt
cargo clippy
cargo test
scripts/release-test.sh
```

Before submitting a pull request, make sure the code is formatted, the tests pass,
and the app builds and runs on your platform.
