# Install Zentop

Zentop has no published release yet, so for now you build it from source. When releases are published, installers will appear on the [GitHub Releases page](https://github.com/amjadjibon/zentop/releases).

## Build from source

You need [Rust](https://rustup.rs) 1.88 or newer.

```sh
git clone https://github.com/amjadjibon/zentop.git
cd zentop
cargo run --release
```

The binary is written to `target/release/Zentop` (`Zentop.exe` on Windows).

### Linux build dependencies

GPUI and the system-tray icon need development headers. On Ubuntu or Debian:

```sh
sudo apt install build-essential pkg-config libfontconfig-dev libfreetype-dev \
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev \
  libvulkan-dev libgtk-3-dev libayatana-appindicator3-dev
```

Other distributions ship the same libraries under similar names.

### Build installers

[cargo-packager](https://github.com/crabnebula-dev/cargo-packager) builds the platform installers into `target/packager`:

```sh
cargo install cargo-packager --locked
cargo build --release
cargo packager --release            # .app/.dmg, setup .exe, .deb, AppImage

cargo install cargo-generate-rpm --locked
cargo generate-rpm                  # .rpm (Linux)
```

## Packages (once released)

| Platform              | Package                                      |
| --------------------- | -------------------------------------------- |
| macOS · Apple Silicon | `silicon-Zentop_<version>_aarch64.dmg`       |
| macOS · Intel         | `intel-Zentop_<version>_x64.dmg`             |
| Windows · x64         | `Zentop_<version>_x64-setup.exe`             |
| Linux · x86_64        | `.deb`, `.rpm` and `.AppImage` for `x86_64`  |
| Linux · ARM64         | `.deb`, `.rpm` and `.AppImage` for `aarch64` |

Each release includes `SHA256SUMS` and a `release-manifest.json` so you can verify downloads.

- **macOS:** open the DMG and drag Zentop to Applications. Unsigned preview builds need a right-click → Open the first time; don't disable Gatekeeper.
- **Windows:** run the setup `.exe`.
- **Linux:** run `uname -m` to check your architecture (`x86_64` or `aarch64`), then install the `.deb` or `.rpm` with your package manager, or make the AppImage executable and run it. AppImages still need FUSE and common system libraries.

## Troubleshooting

- **Blank window on Linux:** check that Vulkan drivers are installed for your GPU (`vulkaninfo` should list a device).
- **No tray icon on Linux:** some desktops hide AppIndicator icons; use **View → Menu-bar Overview** instead.
- **Missing process details:** the operating system hides some details of other users' processes. Zentop does not ask for administrator rights.

Report problems on [GitHub Issues](https://github.com/amjadjibon/zentop/issues) with your OS, CPU architecture, how you installed Zentop, any error text and steps to reproduce. Remove private command lines and environment variables first.
