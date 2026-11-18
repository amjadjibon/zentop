---
date: 2026-11-18
branch: feat/ssh-hosts
reviewer: Codex
verdict: Approve
---

# SSH Hosts code review

## Verdict

**Approve.** Both findings are resolved. The setup guide covers jump-host configuration, and the dashboard now stores hosts separately from shared monitoring preferences.

## Findings

### MED-001. SSH options apply to the destination, while a jump host uses its own configuration

`crates/zentop-gui/src/ssh.rs:23` and `docs/ssh-hosts.md:27`

The connection requires strict host-key checking and batch authentication for the destination. The guide also recommends `ProxyJump`, where OpenSSH starts a separate connection whose options come from the jump host's own configuration. If that alias uses `StrictHostKeyChecking accept-new`, a dashboard connection can accept and save a new jump-host key. The destination still has strict host-key checking. A local `ssh -G` check confirmed that the two aliases resolve different policies, with no network connection. Document the distinction and require each jump alias to use a verified key, strict checking, and batch mode in the setup example. [OpenSSH ProxyJump documentation](https://man.openbsd.org/ssh_config#ProxyJump)

### MED-002. An older GUI or TUI can silently erase saved hosts

`crates/zentop-core/src/monitoring_preferences.rs:69` and `crates/zentop-core/src/monitoring_preferences.rs:256`

The new list is saved in the shared version-1 preferences file. An older client accepts the file, drops `ssh_hosts` during deserialization, and rewrites the entire file when the user edits an alert or saved view. This matters if someone updates the GUI and keeps using a previously built TUI. Store SSH destinations separately from the shared file or use a migration that prevents older clients from silently replacing the new data.

## Resolution

- MED-001. The setup guide now explains the destination and jump-host policies and shows strict checking, batch mode, and disabled agent forwarding on the jump alias.
- MED-002. The host store imports prototype data once into `ssh-hosts.json`. It keeps an empty host file after removing the final entry so legacy hosts stay removed. Regression tests model writes from an older preferences client.

## Verification

- The initial review passed 131 workspace tests. After the fixes, all 133 workspace tests, formatting, strict Clippy, and diff checks passed.
- The delegated desktop smoke test used fake SSH hosts. Live remote hosts and non-macOS desktop builds were outside this review's test environment.
- The main SSH invocation passes the destination as a separate, validated argument and uses a fixed collector command. The reader bounds frame size and retained history. Fixture tests cover retry, disconnect, oversize frames, sequencing, and timeouts.

## Machine-Readable Verdict

```yaml
verdict: Approve
critical: 0
high: 0
medium: 0
low: 0
info: 0
blocking_ids: []
```
