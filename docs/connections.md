# Process network connections

Open a process inspector in the GUI or press **Enter** on a TUI process. TCP rows show protocol, state, local address and remote address. Listening sockets have no remote peer. UDP rows show bound local addresses; UDP peer addresses are unavailable from this backend, including connected UDP sockets. IPv4 and IPv6 endpoints stay numeric; Zentop does not resolve hostnames, inspect traffic, or look up remote IPs online.

In the GUI, **Network connections** takes a snapshot when the inspector opens. **Refresh** replaces it with a new snapshot. The timestamp identifies when it was captured; this section does not refresh automatically. Choose **All**, **TCP** or **UDP**, and use the copy buttons for complete endpoints. GUI snapshots can be refreshed explicitly while general monitoring is paused.

The TUI refreshes the selected process's connections at the sampling interval while its inspector remains open. Pause suspends connection collection; closing the inspector stops it. Queued observations for a different process identity are ignored. Socket collection runs off the UI thread in both frontends.

```sh
cargo run -p zentop-cli -- connections 1234
cargo run -p zentop-cli -- connections 1234 --json
```

The CLI takes a single socket snapshot without the CPU/network warm-up delay. JSON reports include PID, process start time, capture timestamp (Unix seconds), connection rows, and `partial` / `truncated` flags. Missing remote addresses are `null`.

## Scope and limits

- Native socket tables are read through `netstat2` on macOS, Linux and Windows. This feature has been live-tested on macOS; Linux and Windows require platform validation.
- Permissions, sandboxes, containers and process lifetimes can hide sockets. An empty list means **no accessible sockets reported**, not proof of no network activity. No elevated helper is installed and no permission is bypassed.
- Per-entry backend errors set `partial`; initial collection failures are shown as errors. OS/backend omissions may be silent, so a report without `partial` still cannot guarantee complete visibility.
- Start time is checked before and after collection to reject exited/reused PIDs. A snapshot is not an atomic record of all socket activity, and short-lived connections may be missed.
- Rows are sorted and deduplicated; at most 512 unique connections are retained per report. Overflow sets `truncated`. No connection history is retained or written to disk; protocol filtering applies to the retained snapshot.
- TCP listening addresses identify bindings, not firewall reachability. `0.0.0.0` / `::` mean all interfaces; loopback addresses are local to the machine. A connection's direction is not inferred from its ports.
- Unix-domain sockets, byte counts, DNS names and packet capture are outside this view. Existing `:port` searches still match TCP listeners and bound UDP ports, not outgoing remote ports.

Connection exports and copied addresses can contain private network details; redact them before sharing.
