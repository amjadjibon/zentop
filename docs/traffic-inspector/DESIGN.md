---
date: 2026-11-25
feature: traffic-inspector
status: proposed (revised after review)
axes: [system, security, data-model, ui-ux]
---
# Design: HTTP(S) traffic inspector

A Proxyman-style debugging proxy built into Zentop. You point a program at
Zentop's local proxy, and Zentop shows every request it makes: method, URL,
status, headers, body and timing, plus the process that owns the connection.
Linking each request to a process is what Zentop adds over a standalone proxy:
a request opens the same process details sheet as the process list.

Capture is off by default. Until you start it, nothing is recorded, no proxy
listens and no certificate exists.

## Goals

- Inspect HTTP/1.1 and HTTP/2 traffic from programs you point at the proxy.
- Show which process owns each connection, even for HTTPS that isn't decrypted.
- Decrypt HTTPS only for exact hosts you list, in the GUI only, with a root
  certificate whose key never leaves Keychain.
- Keep captured traffic in memory only, under one enforced memory budget.
- Make every change reversible, and never weaken TLS checks toward real servers.

## Non-goals

- **System-wide capture** (changing the Mac's proxy settings). It needs
  privileged, crash-proof restore and a way to authorize browsers; see
  [Later: system-wide capture](#later-system-wide-capture).
- Editing traffic: breakpoints, rewrite rules, Map Local / Map Remote.
- WebSocket frames, HAR export, a Terminal launcher, decryption from the CLI.
- HTTP/3 and QUIC, packet capture, non-HTTP protocols.
- iOS devices and simulators, Linux and Windows. The proxy is portable code,
  but Keychain and attribution are macOS-first.
- Getting past certificate pinning, or diagnosing it.

## Phases

1. **Proxy, tunnels and attribution.** HTTP is recorded in full; HTTPS is passed
   through as blind tunnels (host, port, bytes, timing, process). You route
   traffic by hand, with a per-session token. Traffic window and CLI.
2. **HTTPS decryption (GUI only).** Root certificate with a Keychain-held key,
   trust and removal, exact-host decrypt list, decoded text previews.
3. **Later, each designed separately:** system-wide capture, WebSockets, HAR
   export, a Terminal launcher, decryption from the CLI, editing tools.

Each phase is useful without the next. Phase 1 alone answers "which program is
talking to which host, and how much?"

## How it works

```
 program ──(token)──▶ Zentop front end (127.0.0.1:<port>)
                          │  auth · policy · attribution · byte counts
                          ├─ HTTP request ───────────────▶ recorded, forwarded
                          ├─ CONNECT, host not listed ───▶ blind tunnel, recorded as metadata
                          └─ CONNECT, host listed (phase 2) ─▶ TLS terminated with a per-host
                                                              certificate, recorded, forwarded
                                                              with OS-verified TLS
```

### Front end

Zentop owns the front end itself: accepting connections, authorization,
destination policy, attribution, tunnelling and byte counting. Only streams
chosen for decryption are handed to the MITM engine. The review found that
`hudsucker`'s stock CONNECT path swallows tunnel errors and byte counts, so
those can't depend on it.

- **Listener.** `127.0.0.1` only, on a port the OS assigns unless you set one.
  The window and CLI print the bound address. IPv6 loopback isn't served in v1,
  so attribution has one address family to handle.
- **Per-session token.** Each capture session gets a random token. Clients must
  send it as proxy credentials: `http://zentop:<token>@127.0.0.1:<port>`.
  - Requests without it get `407 Proxy Authentication Required`.
  - The `Proxy-Authorization` header is consumed by the proxy and is never
    forwarded, recorded or shown.
  - This stops other local users, and web pages, from using the proxy.
- **Only proxy requests.** Requests in origin form (`GET / HTTP/1.1` sent
  straight to the port, as a web page hitting `localhost` would) are rejected
  with `400` and not recorded.
- **Destination policy.** The check runs on the address Zentop actually dials,
  after DNS resolution, not on the host name the client sent.
  - Loopback, link-local and private ranges, and the proxy's own address, are
    refused by default.
  - "Allow local destinations" turns this off for development, per session.
  - The same rules apply to plain HTTP, CONNECT, and anything upgraded.
- **Limits.** At most 256 open client connections. Beyond that, new
  connections get `503` and a counter shows how many were refused.

### Decryption boundary (phase 2)

A CONNECT request names a host, the TLS handshake names a host (SNI), and each
request inside names a host (`Host`, or `:authority` for HTTP/2). These can
differ, so the boundary is enforced at every step:

1. Canonicalize each name: lowercase, IDNA to ASCII, strip one trailing dot,
   make the port explicit.
2. Decrypt only if the CONNECT host is on the decrypt list **and** not on the
   skip list. Both checks happen before any certificate is signed.
3. The SNI must equal the CONNECT host, or the handshake is refused.
4. Each inner request's authority must equal the CONNECT host, or that request
   is refused with `421 Misdirected Request` and recorded as refused.
5. CONNECT inside a decrypted connection is refused.

The decrypt list holds **exact hosts** only (`api.example.com`). Wildcards can
come later, once this matching is proven.

The skip list (Apple account, update and other pinned services) is best-effort
protection for well-known sensitive hosts, not a guarantee. The UI says so.

### Upstream TLS

- Zentop verifies servers with `rustls-platform-verifier`, which uses the macOS
  trust store and its policies. The verifier configures the upstream
  `tokio-rustls` connector. Hostname and certificate checks are always enabled.
- An upstream certificate failure fails the request toward the program, and
  the row shows the upstream error.
- HTTP/2 is enabled explicitly and tested on both sides: program to Zentop, and
  Zentop to server.
- **WebSockets.** In tunnels they work, because tunnels are blind. On a
  decrypted host, an upgrade request fails with a clear row ("WebSockets on
  decrypted hosts aren't supported yet; remove the host from the decrypt list").
- If a program rejects Zentop's certificate, the row says "The program rejected
  Zentop's certificate". It doesn't guess why (pinning, untrusted root, or
  something else).

### Crate layout

A new crate, `crates/zentop-proxy`, holds the front end, certificate authority,
attribution worker and flow store, with no UI code. It joins the workspace
`members`. The GUI and CLI depend on it, macOS-gated, so it's part of the
default GUI build on macOS. `zentop-core` and the TUI don't use it.

| Piece | Implementation |
| --- | --- |
| Front end | Zentop's own, on `hyper` 1 + `tokio` |
| TLS termination (phase 2) | `tokio-rustls` for both TLS legs, with `hyper` for HTTP/1.1 and HTTP/2 |
| Certificates | `rcgen` 0.14; one fresh ECDSA P-256 key per host certificate |
| Root key | `security-framework`: a non-exportable Keychain `SecKey` that signs via rcgen's `SigningKey` trait |
| Upstream TLS | `rustls` + `rustls-platform-verifier` |
| Attribution | Socket table via `netstat2`, after fixing its macOS backend (see Attribution) |
| Runtime | One tokio runtime on a dedicated thread; GUI and CLI talk to it over bounded channels |

**Engine choice.** Zentop terminates TLS at the authorized CONNECT upgrade
using `tokio-rustls` and serves the stream with `hyper`. The existing front
end continues to own attribution, capture, destination policy, errors and
tunnels. `LazyConfigAcceptor` exposes the ClientHello, so Zentop enforces SNI
before asking Keychain to sign a leaf. The stock hudsucker CONNECT path would
require adapting its handoff, handshake error reporting and capture ownership
to provide the same controls.

**Toolchain.** At the time of review, every named crate declared a minimum
Rust version at or below the workspace's floor. The workspace floor tracks
current stable Rust.

## Certificates and trust (phase 2)

### The root certificate

- Created the first time you turn on decryption: ECDSA P-256, valid for 1
  year, named "Zentop Local Proxy CA (<machine name>)", constrained to TLS
  server authentication. Every install gets its own.
- Its private key is created inside Keychain as a **non-exportable** `SecKey`
  and never exists in memory or on disk outside Keychain. Zentop signs host
  certificates by asking Keychain to sign. `hudsucker`'s built-in
  `RcgenAuthority` isn't used, because it reuses the root key in every host
  certificate.
- The root is identified everywhere by its SHA-256 fingerprint.

### Host certificates

- Each host gets its own freshly generated key, never the root's.
- Valid for 7 days, held in memory only, cached in an LRU of 256 hosts.

### Trust

Trusting the root is a separate, explicit step:

```sh
security add-trusted-cert -r trustRoot -p ssl -k ~/Library/Keychains/login.keychain-db zentop-ca.pem
```

- This is the user trust domain (no `-d`), limited to the SSL policy (`-p ssl`).
  macOS shows its own authentication dialog.
- Zentop never edits system-wide trust.

### States the UI shows

The setup section stays visible even when capture is off, so an installed root
is never forgotten.

| State | Shown as | Action offered |
| --- | --- | --- |
| None | "No certificate" | Create |
| Created, not trusted | "Created · not trusted" | Trust, Remove |
| Trusted | "Trusted · expires <date>" | Remove |
| Key unavailable (Keychain locked, or access lost after an update) | "Certificate key unavailable" | Unlock and retry, or Replace |
| Expiring within 30 days | "Expires <date>" | Replace |
| Partly removed | Lists what's left: trust, certificate or key | Retry removal, manual steps |

- **Cancelled prompts** leave the previous state unchanged.
- **Removal** deletes the trust setting, the certificate and the key, then
  checks each one separately. A part that couldn't be removed is reported with
  the `security` command to remove it by hand.
- **Replacing** creates and trusts the new root first, and only then removes the
  old one. An interrupted replacement leaves the old root working.
- **Signing and Keychain access.** Keychain ties the key to the app's code
  signature. Release DMGs are currently ad-hoc signed, so an update may lose
  access to the key. That shows up as "Certificate key unavailable", and
  Replace fixes it. Whether signed releases keep access across updates is
  verified before promising it (see Open questions).

## Process attribution

Attribution is **best-effort**. It labels the process that owns the program's
end of the connection. That may be a helper rather than the app you think of,
such as a browser's network process. The GUI's helper grouping already links
helpers to their app.

**How a connection is matched.**
- A connection accepted on `127.0.0.1:<port>` from `127.0.0.1:<client>`
  matches an established TCP socket whose local address is `127.0.0.1:<client>`
  and whose remote address is `127.0.0.1:<port>`. Both endpoints must match.
- The result is stored against the connection's ID, not the address, because
  addresses are reused.

**Ambiguity is reported as unknown, never guessed.**
- A connection stays "Unknown process" when:
  - no socket matches;
  - more than one process holds the socket (shared descriptors);
  - the owner exited before the scan;
  - permission was denied, as for other users' processes.
- Attribution can't use the `(pid, start_time)` check from
  `connections::collect`, because that check needs the PID up front. Instead,
  the macOS unique process ID is read right after the match and stored with the
  flow.
- `(pid, start_time)` is kept only for looking the process up in the GUI's
  list, because `sysinfo` start times are in whole seconds.

**Cost.**
- A worker runs only while capture is on. It batches pending connections and
  scans immediately when idle, with at least 25 ms between scan starts and a
  bounded queue. Flows over the limit stay unknown.
- The first authenticated request on each connection waits for attribution
  before dialing, up to 250 ms. The flow records that wait. Later requests
  reuse the connection's attribution without waiting.
- Before repeated scans ship:
  - Fix the `netstat2` macOS backend: version 0.11.2 treats a zero return from
    `proc_pidfdinfo` as success, but zero means failure. Fix it upstream, or
    vendor a patched copy.
  - Benchmark a scan on a busy machine.
- Tests cover short-lived clients, shared descriptors and other users' sockets.

## Data model

```rust
pub struct Flow {
    pub id: u64,                        // monotonic within a session
    pub connection: u64,                // the client connection it arrived on
    pub started: SystemTime,
    pub process: Attribution,           // Known { unique_id, pid, start_time, name } | Unknown(reason)
    pub kind: FlowKind,                 // Http | DecryptedHttps | Tunnel
    pub request: Option<RequestHead>,   // method, url, version, headers (redacted copy stored)
    pub response: Option<ResponseHead>, // status, headers
    pub request_body: Body,             // Captured(Bytes) | Truncated { kept, total } | Skipped(reason)
    pub response_body: Body,
    pub timing: Timing,                 // connect, tls, ttfb, total
    pub bytes: (u64, u64),              // sent, received; tunnels included
    pub state: FlowState,               // Active | Complete | Refused(reason) | Failed(reason)
}
```

### Memory budget

One shared budget of **64 MB** covers everything captured: request lines,
headers and bodies. It's charged **as bytes stream in**, not when a flow
finishes.

| Limit | Value | When exceeded |
| --- | --- | --- |
| Flows kept | 2,000 | Oldest flow dropped |
| Headers per message | 64 KB | Rest dropped, flow marked |
| Body kept per message | 1 MB | Rest counted, not kept (`Truncated`) |
| Shared budget | 64 MB | Oldest **bodies** dropped first, keeping their metadata; if the budget is still full, new bodies are `Skipped("memory limit")` |
| Text preview | 256 KB decoded | Computed on demand, never stored |
| Event queue to the UI | 1,024 | Updates coalesced; a counter shows what was dropped |

Streams that never end (SSE, long polls) keep at most their first 1 MB, like
any body.

### Session controls

- **Pause:** traffic keeps flowing and is counted, but isn't recorded.
- **Clear:** drops completed flows. Flows still in progress keep going but stop
  capturing bodies.
- **Stop:** no new connections are accepted, open ones are closed within 2
  seconds, then the store is freed.

## Redaction

One redaction policy applies everywhere a flow can leave the store: the list,
the detail pane, copy buttons and CLI output.

- `Proxy-Authorization` is stripped before capture and never stored.
- **Masked by default, shown on request:**
  - the values of `Authorization`, `Cookie` and `Set-Cookie`;
  - query values whose names look secret (`token`, `key`, `secret`,
    `password`, `sig`, `code`, `auth`).
  - In the detail pane, a "Show" control reveals them for that flow only.
- **Copy as cURL** leaves masked values out unless you choose "Include secrets".
- Bodies and custom headers can still hold secrets. The UI warns about this
  next to the copy controls, rather than claiming they're safe.
- **Copying and CLI output are the only ways captured data leaves memory.**
  The docs say so.

## Interface

### Settings and ownership

- **Who owns capture.** Capture belongs to the app, not the Traffic window.
  Closing the window leaves capture running. The menu-bar overview shows
  "Capturing · <address>" with a Stop button. Quitting Zentop stops capture.
- **What's saved.** The decrypt list and an optional fixed port live in the
  GUI's `settings.json`, in the config directory (`ZENTOP_CONFIG_DIR` if set).
- **What's never saved in app settings:** tokens, flows and private keys.
  A separate recovery record saves the public certificate and Keychain item
  identifier. The file lets Zentop check trust and finish removal even if one
  Keychain item is missing.

### GUI: Traffic window

A separate window, opened from **View → Traffic** or the menu-bar overview,
following the SSH Hosts window pattern.

- **Header.** Start/Stop; the bound address; the session token with copy
  buttons; and ready-made routing lines:
  ```sh
  export http_proxy=http://zentop:<token>@127.0.0.1:<port>
  export https_proxy=$http_proxy
  curl -x "$http_proxy" https://example.com
  ```
  Lowercase `http_proxy` is used because curl ignores `HTTP_PROXY`.
- **Toolbar.** Search; filters (process, host, method, status class, kind, has
  error); Pause; Clear; counters for refused, dropped and unattributed traffic.
- **Flow list.** A virtual table: time, process (icon + name), method, host,
  path, status, size, duration.
  - Errors and 4xx/5xx statuses use the theme's warning and danger colours.
  - Tunnel rows are dimmed and say "Not decrypted"; in phase 2 they offer
    "Decrypt this host".
- **Detail pane.** Request, Response and Timing tabs.
  - Headers appear as a table, after redaction.
  - Bodies show as a decoded text preview (gzip, br and deflate are
    decompressed; JSON is pretty-printed), or as hex for binary data.
- **Copy.** URL, Copy as cURL, a header, or the body, all through the existing
  `copy_button` helper and the redaction policy.
- **Process link.** The process name opens its details sheet. The details sheet
  gains "Show traffic", which opens Traffic filtered to that process.
- **Certificate and hosts.** Shows the certificate state and actions,
  including public PEM export and exact-host list management.

### CLI

```sh
zentop-cli traffic                 # OS-assigned port; prints the address and token
zentop-cli traffic --port 9090     # a fixed port
zentop-cli traffic --json          # one JSON object per flow
zentop-cli traffic --headers       # include redacted headers
```

- Output is metadata only by default: method, host, path without its query,
  status, size, timing and process.
- The CLI never decrypts and never changes system settings.

### TUI

Not planned. The flow store doesn't depend on any UI, so a TUI view can be
added later.

## Failure handling

| Situation | Behaviour |
| --- | --- |
| Fixed port in use | Capture doesn't start; the error names the port and offers an OS-assigned one |
| Missing or wrong token | `407`, not recorded; the refused counter goes up |
| Origin-form request | `400`, not recorded |
| Destination refused by policy | `403` with the reason; recorded as `Refused` |
| SNI or authority mismatch | Handshake refused, or `421`; recorded as `Refused` |
| Upstream TLS error | Request fails toward the program; the row shows the reason |
| Program rejects Zentop's certificate | Row says so; no guess about why |
| Keychain locked, or key unavailable | Decryption is off for the session; tunnels keep working; the certificate section says why |
| Proxy task panics | Release builds abort the whole app (`panic = "abort"`). Phase 1 changes no system settings, so nothing needs restoring. The front end avoids panics on input: parse errors become `400` |
| Memory budget reached | Bodies dropped as described; counters show it |

## Privacy and security summary

- Off by default. The proxy does not change network settings. Certificate
  trust changes require explicit approval in the user domain.
- Loopback listener with a per-session token, plus a destination policy, so
  neither other users nor web pages can use the proxy, or reach your local
  network through it.
- TLS toward servers is always verified by macOS. The root key can't be
  exported. Host certificates have their own keys.
- Captured traffic stays in memory, under a hard budget. It leaves only through
  copying or CLI output.
- **Docs to update.**
  - `docs/usage.md` says Zentop makes no network requests. It needs a "Traffic
    inspector" note: when you start capture, Zentop resolves host names and
    forwards requests for the programs you route through it.
  - The README's Privacy section, which already covers SSH Hosts, gets the
    same note.

## Testing

- **Unit:**
  - name canonicalization and decrypt/skip matching;
  - token checks, origin-form rejection, destination policy (including names
    that resolve to private addresses);
  - budget accounting while streaming, eviction order;
  - redaction across list, cURL and CLI;
  - the certificate state machine.
- **Integration** (on ephemeral ports, with no system changes):
  - Drive a local `hyper` server through the proxy over HTTP, tunnelled HTTPS,
    and (phase 2) decrypted HTTPS using a test root and a test verifier.
  - Cover HTTP/1.1 and HTTP/2 on both sides; SNI/CONNECT/authority mismatches;
    nested CONNECT; an upstream certificate error; and a stream larger than
    the budget.
- **Attribution:**
  - Connect from the test process and check it resolves to
    `std::process::id()`.
  - A short-lived client that exits before the scan resolves to Unknown.
  - Run the same test over IPv6 if an IPv6 listener is ever added.
- **Manual QA (macOS):**
  - Create, trust, replace and remove the certificate, including with the
    Keychain locked and with each prompt cancelled.
  - Update from one build to the next and check key access.
  - Route curl, a Node script and Safari (configured by hand) through the
    proxy; check a pinned app is unaffected unless its host is listed.

## Later: system-wide capture

Deferred until designed separately. Requirements the review established:

- **Authorized changes.** Change settings through `SCPreferencesCreateWithAuthorization`
  in one transaction, keyed by stable network-service IDs, with a single owner
  at a time. `networksetup` needs admin, and sometimes root, rights.
- **A restore journal.** Saved before any change: private, written atomically,
  integrity-protected (an HMAC keyed from Keychain), and kept after a partial
  failure.
- **Conditional restore.** Restore a setting only if it still equals what
  Zentop applied; never overwrite a change someone else made since.
- **Configurations to leave alone.** Refuse to touch services that use PAC or
  WPAD, managed (MDM) proxies, or authenticated proxies.
- **Crash recovery.** Recover without relying on cleanup code, because release
  builds abort on panic. Options: a small launchd watchdog, or restore on next
  launch with a clear warning. Ownership is checked against the journal's
  integrity data, never a PID alone.
- **Offline restore.** Document manual restore steps that work without
  Zentop: System Settings, and the equivalent commands.
- **Browsers and the token.** Browsers can't easily send a proxy token, so
  system routing needs its own client-access policy before it can ship.
- **Verify browser behaviour.** Check Safari and Chrome on supported macOS
  versions with a working proxy, an unreachable proxy, and bypass rules,
  before claiming what gets captured.

## Decisions

- **Manual routing with a token for v1, over system-wide capture.** No
  privileged changes and nothing to restore after a crash, and the token
  closes the proxy to other users and web pages.
- **Zentop owns the front end, over the stock `hudsucker` CONNECT path.** It
  needs tunnel byte counts, attribution, authorization and error reporting
  that the stock path doesn't expose.
- **A custom certificate authority, with a Keychain-held key and per-host
  keys, over `RcgenAuthority`.** The root key is never copied into TLS
  configurations or host certificates.
- **Exact hosts, with the decrypt check at CONNECT, SNI and every request,
  over wildcards and a single check.** The decryption boundary can't be
  side-stepped by changing a host name partway through.
- **A separate `zentop-proxy` crate.** TLS and async networking stay out of
  `zentop-core` and the TUI.
- **Memory-only capture with one streaming-charged budget.** It matches
  Zentop's "no history on disk" rule for connections, and stays bounded under
  load.
- **An OS-assigned port by default.** No clash with Proxyman, Charles or dev
  servers; a fixed port is available when you need one.
- **A separate Traffic window**, following SSH Hosts.

## Open questions

- **Keychain access across updates.** Phase 2 uses the file-based macOS
  Keychain. `SecKeyCreateRandomKey` needs the extractability flag in the
  top-level dictionary for this backend. The temporary-keychain test checks
  that private-key export fails and verifies a leaf using its public root.
  On this Mac, rebuilding the ad-hoc executable changed its code-directory
  hash. After the isolated keychain was unlocked, macOS still prompted the
  rebuilt app for signing access. Denying the prompt left the key unavailable.
  Authorizing the rebuilt app restored signing and the existing PEM still
  verified the connection. Continuity for Developer ID-signed updates
  remains unverified. **Unlock and retry** checks the current key and
  **Replace** rotates the root if access cannot be restored.
- **Development isolation.** Development builds accept `ZENTOP_KEYCHAIN`
  to select an isolated file keychain. User-domain trust changes are disabled
  in this mode because they would apply beyond the file. Tests use a mock
  trust runner with real items in a temporary keychain.
- **`netstat2` fix.** Fix it upstream, vendor it, or call `libproc` directly
  for this one lookup?
