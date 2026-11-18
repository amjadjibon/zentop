# SSH Hosts

Open **SSH Hosts** from the Zentop desktop title bar or the **View** menu.
The window displays CPU, RAM, disk usage, uptime, and reading age for Local and
up to 12 configured remote machines. Select a remote host to see up to eight
recent readings, each showing up to 24 processes ranked by CPU.

## Set up a host

1. In a terminal, configure an OpenSSH destination and verify its host key
   against a trusted fingerprint. Zentop uses your existing SSH configuration,
   keys, local agent, and known-hosts files. Authentication must work without
   an interactive prompt.
2. Build the matching `zentop-cli` on the remote machine with
   `cargo build --release --locked -p zentop-cli` and make the binary available
   as `zentop-cli` on the remote non-interactive SSH command PATH. Run it as the
   usual SSH user.
3. Check that the destination returns one JSON line with this terminal command.
   Substitute your alias for `home-server`.

   ```sh
   ssh -o BatchMode=yes -o StrictHostKeyChecking=yes home-server 'zentop-cli stream --protocol 1 --samples 1'
   ```

4. In Zentop, choose **Add host**, enter a display name and the destination,
   then choose **Save and connect**.

Enter an SSH alias, hostname, IP address, or `user@host`. For custom ports,
identity files, and jump hosts, use an alias from your OpenSSH configuration.
For example, an alias for a host on port 2222 can look like this.

```sshconfig
Host home-server
    HostName server.example.invalid
    User observer
    Port 2222
```

### If you use a jump host

Zentop requires strict host-key checking and batch mode for the destination.
OpenSSH opens a separate connection to each `ProxyJump` host using that host’s
own SSH configuration. Verify each jump host’s key against a trusted
fingerprint and configure its alias explicitly.

```sshconfig
Host lab-bastion
    HostName bastion.example.invalid
    User observer
    StrictHostKeyChecking yes
    BatchMode yes
    ForwardAgent no

Host lab-vm
    HostName vm.example.invalid
    User observer
    ProxyJump lab-bastion
```

## Connections and readings

- Zentop requires an already-trusted host key for the destination and runs SSH in batch mode. It
  invokes the fixed command `zentop-cli stream --protocol 1` with agent
  forwarding, other SSH forwards, and SSH connection sharing disabled.
- Readings normally arrive every three seconds. Disk usage follows Zentop's
  existing storage metric, using the root filesystem on Unix and aggregate
  disk capacity on Windows. An unavailable capacity appears as a dash.
- **Disconnect** keeps the host and its last readings in the window.
  **Connect** resumes the stream. An unexpected drop keeps the last readings
  visible and retries automatically with a delay of up to 30 seconds.
  **Retry now** starts another attempt immediately.
- **Remove host** deletes the saved destination and closes its stream.
  Closing the SSH Hosts window stops its sessions. Reopening the window
  connects to saved destinations again. Only display names and destinations
  persist in `ssh-hosts.json`, in Zentop’s configuration directory. The first
  time this dashboard opens, it imports any hosts saved by the prototype in
  `monitoring.json`. From then on, `ssh-hosts.json` is the source of truth.
- Readings contain resource totals and process names, PIDs, CPU, and RAM.
  The remote view offers read-only inspection. This release keeps history in
  memory for the life of the window. Long-term history, per-volume remote
  breakdowns, and remote alerts are outside this release.

If the first reading never arrives, check OpenSSH availability, host-key
trust, non-interactive authentication and the remote PATH. Remote shell
startup output must leave stdout clear for the JSON stream.

## Stream protocol

Protocol 1 writes newline-terminated JSON objects to stdout. The CLI warms up
the shared sampler before the first reading, flushes each line, and exits after
1,200 samples or when its output pipe closes. The dashboard renews completed
sessions. Use `--samples 1` through `--samples 1200` to end a diagnostic run
after a specific number of readings.

Each object carries `version`, a one-based session `sequence`,
`captured_at_ms`, `metrics`, and `processes`. The receiver limits each line
to 32 KiB, checks the sequence and schema, and measures freshness from local
receipt time to avoid clock skew.
