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

### Build checks and releases

Pull requests and pushes to `main` run formatting, release-tool tests, Rust tests,
and package builds for Windows x64, macOS Intel/Apple Silicon, and Linux x64/ARM64.
Linux packages are built on Ubuntu 22.04 with cargo-packager (deb, AppImage) and
cargo-generate-rpm (RPM). AppImages are checked for bundled Wayland client
libraries to prevent the display-driver conflict reported in issue #203.

To build preview packages, run **Build Check** or **Draft Release** from GitHub
Actions on a branch. Download the `packages-*` workflow artifacts. Preview macOS
packages are ad-hoc signed and are not notarized for public distribution.

For a release candidate:

1. Update the version in `Cargo.toml` and run `cargo check` to refresh `Cargo.lock`.
   Run `scripts/release.sh check-version` and commit the changes.
2. Push a matching version tag, for example `v1.3.0`. All platform packages are
   built from that tag. A published version cannot be overwritten.
3. After every build passes, **Draft Release** verifies all nine packages and
   attaches `SHA256SUMS` and `release-manifest.json` to a draft GitHub release.
4. Install and launch the packages on their supported operating systems, review
   the release notes, then publish the draft from GitHub when ready.

Tagged macOS builds require the repository secrets `MACOS_CERTIFICATE`
(base64-encoded Developer ID `.p12`), `MACOS_CERTIFICATE_PASSWORD`, `APPLE_ID`,
`APPLE_PASSWORD` (an app-specific password), and `APPLE_TEAM_ID`, plus the
repository variable `APPLE_SIGNING_IDENTITY` naming your Developer ID
Application identity. Tagged builds fail if credentials are missing or signature/notarization
checks fail. Preview builds and pull requests do not need signing credentials.
Windows packages are NSIS installers, named `Zentop_<version>_x64-setup.exe`;
Windows code signing is not configured by this workflow.

The release uses the scoped `GITHUB_TOKEN`; `PAT_TOKEN` is no longer needed.
Existing branch-protection rules that require the old **Format Check** job should
be updated to require **Source checks** and the platform package jobs.

### Code Formatting and Checks

```bash
cargo fmt
cargo clippy
cargo test
scripts/release-test.sh
```

Before submitting a pull request, make sure the code is formatted, the tests pass,
and the app builds and runs on your platform.
