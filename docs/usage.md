# Using Zentop

Zentop is a free, open-source system monitor for macOS, Linux and Windows. It runs entirely on your machine: no account, no cloud service and no background agent.

## Find the process you need

Use **⌘F** (macOS) or **Ctrl+F** to focus search. Search by name, command, or PID; use commas to match any term and regular expressions for names. Try `node, python`, `^kernel`, or `d$`. A bare number matches a PID. Use `:3000` or `port:443` to find a local TCP listener or a bound UDP socket. Sockets are collected only while an explicit port query is active; permissions can hide other users’ sockets. Resume monitoring before searching ports.

**Filters** combines CPU, GPU engine activity, RAM, runtime and status conditions. Active conditions and matching counts appear below the toolbar; **Clear all** restores the complete list. GPU filters exclude unavailable readings rather than treating them as zero.

Choose **List** for individual processes or **Tree** for families. Tree search retains parents and expands matching branches; family CPU and RAM continue to include hidden descendants. CPU may exceed 100% across cores, and shared pages can overlap in summed family RAM. Expanded families stay together when paging.

Pin a process with the pin action, inspect it with the information action, or use the end action to request termination after confirmation. The details sheet lists the ports a process listens on (TCP) or has bound (UDP), and whether each accepts connections from other machines or only from this computer; ports are checked when the sheet opens. **Columns** chooses what the table shows; process names always stay visible. Choose rows per page and move between pages below the table. The Actions column stays pinned on the right while the other columns stretch with the window and scroll sideways when they do not fit.

## Explore resources

Use each card’s arrow to open a detailed sheet. CPU, memory, GPU and network histories cover roughly two minutes in memory. They are cleared on restart and are not long-term monitoring storage. Gaps indicate paused, missing or delayed readings.

- **CPU:** average utilization, individual logical cores, and hardware identity. Process CPU can exceed the 100% scale of a single core.
- **Memory:** used RAM, actual free pages, swap and breakdowns exposed by the OS. “Not in use” on the overview means total minus used and differs from strictly free pages.
- **Graphics:** select an adapter and inspect utilization, memory, temperature, core count, power and core clock where the driver exposes them. Apple silicon shared GPU memory is already part of system RAM.
- **Storage:** capacity and mounted volumes. Unknown/zero capacity stays unavailable. **Open DiskTree…** opens the read-only [DiskTree treemap](disktree.md) for directory-level usage.
- **System:** OS/build, CPU architecture, uptime and load averages. Load average is not CPU percentage.
- **Network:** received/sent rates, totals and interface details. Interfaces with traffic are listed first. New/reset counters stay unavailable until a valid rate baseline exists. This is network I/O monitoring, not a firewall or packet inspector.

GPU support is driver dependent: macOS IORegistry statistics; Linux AMD sysfs and NVIDIA NVML; Windows NVIDIA NVML with DXGI inventory for other adapters. Missing counters never imply idle hardware. The optional **GPU %** process column and filter use Linux DRM engine accounting. They need two samples and may exceed 100% across engines; unsupported or unreadable clients remain unavailable.

## Menu-bar companion

Open the Zentop ring icon in the menu bar/system tray or **View → Menu-bar Overview** for CPU, RAM, GPU, network, free storage and the five busiest processes. Select a process to open its inspector in the main app. The companion and main window share a sample, interval, theme and pause state. Sampling pauses while no Zentop window is open.

Linux tray behavior varies with the desktop; use **Show overview** in the tray menu or **View → Menu-bar Overview** if clicking the icon does not open it.

## Settings and appearance

Open **Settings** from the application menu or **⌘, / Ctrl+,**. General holds the refresh interval, tray icon and keep-running choice. Changes are saved automatically in the app configuration directory and synchronize across windows. Existing preferences carry over; save failures are shown in Settings.

The default is to quit when the main window closes. Enable **Keep running when the window closes** to continue through the tray; this requires the tray icon. **Quit Zentop** always exits. Closing Settings or About leaves monitoring running.

Settings → Appearance and the title-bar palette menu share all 24 original themes; a change applies to every open window at once.

## Terminal apps

Run `cargo run -p zentop-cli -- system` for a system summary, or
`cargo run -p zentop-cli -- processes --sort memory --limit 10` for a process list.
Add `--json` for machine-readable output. Reports take two observations, 1.5
seconds apart, to establish CPU and network rates.

Run `cargo run -p zentop-tui` for a terminal dashboard with the GUI's six
resource cards: CPU, Memory, GPU, Storage, System and Network. Wide terminals
show one row of cards; narrower terminals wrap them or show a compact summary.
CPU, RAM, GPU and network cards include two-minute trends with gaps for missing
readings. Press **1–6** for resource details.

- **/** or **Ctrl+F**: search by name, command, PID, regular expression or `:port`.
  Enter keeps the search; Esc clears it. **x** clears search and filters.
- **f**: filters. Up/down chooses a rule; Space enables it; left/right adjusts
  the threshold; **o** changes the comparison. The last row chooses a status.
- **t**: List/Tree; left/right collapses/expands families. Tree totals and search
  follow the same rules as the GUI, including app-owned helpers on macOS.
- **Space**: pin/unpin a command. **Enter** opens process details; **e** reveals
  environment values in that sheet. Port collection runs only for port searches
  or an open process sheet; paused port lookups ask you to resume.
- **c / m**: sort by CPU/RAM. **[ / ]** chooses a sort column; **r** reverses it.
  **v** chooses columns; narrow terminals hide columns that do not fit.
- **p**: pause/resume sampling. **,** opens interval settings. **T** cycles all
  24 GUI palettes. **?** lists shortcuts.
- **Up/down** or **j/k**: scroll. **q** or **Ctrl+C** quits; in a sheet, **q** or
  **Esc** closes the sheet first.

`cargo run -p zentop-tui -- --interval 5000 --theme dracula` starts with a
five-second interval and the Dracula palette. Intervals range from 1500 to
30000 milliseconds. TUI pins, filters, columns, interval and theme choices last
for that terminal session; they do not change GUI preferences. Process
termination and clipboard buttons remain GUI features. The TUI needs an
interactive terminal; use the CLI for redirected output. Both apps support
`--help` and `--version`.

## Disk usage treemap

Open **View → DiskTree…** in the GUI, press **d** in the terminal monitor, or run
`cargo run -p zentop-tui -- --disk /path/to/folder`. The [DiskTree guide](disktree.md)
covers navigation, modes, marked review and partial-scan limits.

## Installing and support

See the [installation guide](install.md). Include OS, architecture, app version, package type and reproduction steps in [bug reports](https://github.com/amjadjibon/zentop/issues). Redact private command lines, environment variables and identifiers before sharing diagnostics or screenshots.

## Privacy

Zentop sends nothing. There are no analytics or launch counts, and no network requests; everything it reads stays on your machine.
