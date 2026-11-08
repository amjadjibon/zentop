# Changelog

Zentop is a fork of [NeoHtop](https://github.com/Abdenasser/neohtop) 1.3.0, rebuilt as a native Rust app with GPUI in place of Tauri.

## Unreleased

- Copy buttons for a process's PID, name, command, path, ports and environment variables, for network interface names, and for version and system information (System details and About), ready to paste into a bug report.
- Process details show the ports a process listens on, with protocol and whether each is reachable from other machines or only this computer.
- On macOS, helper processes now count toward the app they work for, such as a webview's WebKit content, GPU and networking processes. Tree view nests them under the app with combined CPU and memory, and process details show a **Process family** total and a **Belongs to** link.
- New name, **Zentop**, with its own settings folder and bundle identifier.
- New app icon and menu-bar glyph: an open ensō ring with a dot in its gap. Sources live in `assets/brand/`.
- Rebuild the desktop app natively in Rust with GPUI Kit, replacing Tauri, Svelte and the webview. All windows (monitor, Settings, About and the menu-bar overview) share one sampling loop and one settings file.
- Ending a process now confirms it is still the same process (by start time) before signalling it.
- Remove analytics entirely: Zentop makes no network requests.
- Remove the website sources and all Node.js tooling; release packaging uses cargo-packager and cargo-generate-rpm.
