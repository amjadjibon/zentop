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
- Editing traffic before phase 3B (see [Phase 3B: live editing](#phase-3b-live-editing)).
  The CLI never edits traffic.
- A Terminal launcher and decryption from the CLI.
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
3. **Phase 3A:** capture navigation, request replay, HAR export and WebSocket messages.
4. **Phase 3C:** a dense, professional HTTP-debugger window (see below).
5. **Phase 3B:** breakpoints, Map Local, Map Remote and rewrite rules (GUI only).
6. **Phase 3D:** Open with Zentop. Zentop launches Chromium browsers and
   Electron apps pointed at its proxy and authorizes them by process family,
   without the token and without changing system proxy settings (GUI only).
7. **Later, each designed separately:** system-wide capture, a Terminal
   launcher, and decryption or traffic editing from the CLI.

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
  - Phase 3D adds one other way in: the process family of an app Zentop
    launched (see [Client-access policy](#client-access-policy)).
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
- **WebSockets.** Blind tunnels relay opaque bytes. Phase 3A inspects HTTP/1.1
  upgrades after TLS termination and the inner authority check.
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
- Data leaves memory through explicit copy, replay, HAR export or CLI output.

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
  explicit copy, replay, HAR export or CLI output.
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

## Phase 3A

- **Navigation.** A collapsible Apps and Domains sidebar uses live store counts,
  including unknown processes. Related hosts group by public-suffix-aware
  registrable domain. A sidebar selection combines with the existing filters.
  The virtual list supports eight sortable columns and keyboard selection.
  The list and details share a resizable split.
- **Repeat and Edit and resend.** The GUI submits a retained HTTP request to
  the active engine, which records a new flow attributed to Zentop. Replay
  applies the same destination checks and platform TLS verification. HTTPS
  requires an exact decrypt-list entry and honors the skip list. The listener
  still requires its session token. Repeat uses the privately retained
  originals, while the editor masks secrets through the shared policy and
  preserves untouched values. Incomplete requests and tunnels cannot replay.
- **HAR 1.2.** An explicit save action streams the selected flow or matching
  flows to disk. Shared redaction applies by default, bodies are opt-in with
  a captured-traffic warning, and tunnels export as CONNECT without content.
  Unknown timing phases use the HAR unknown value.
- **WebSocket messages.** Plain HTTP and decrypted HTTP/1.1 upgrades relay the
  original bytes and inspect copies in both directions. Decrypted upgrades
  enforce the CONNECT authority and verify TLS upstream. Captured messages
  handle masking, continuation and negotiated permessage-deflate. Per-connection
  limits cap retained frames and payload bytes, and the shared 64 MB budget
  charges retention. The virtual Messages tab warns that payloads may contain
  secrets. Messages stay in memory and blind tunnels remain opaque.

## Phase 3C: debugger window

The phase 1–3A window works but spends most of its height on setup, shows
about ten tall rows, and puts request and response behind one set of tabs.
Phase 3C turns it into a dense, professional HTTP-debugger layout of the kind
macOS developers expect from a dedicated tool, in Zentop's own name, theme and
icons. No behaviour, limit or security rule from earlier phases changes.

### Layout

```
┌ sidebar ─────┬ toolbar: ⏸  ✎  🗑     ● Capturing · 127.0.0.1:54139 ▾      ⌘  ◧  ▭  ◨ ┐
│ Favorites    │ All HTTP HTTPS WebSocket │ JSON Form XML JS CSS GraphQL Document Media Other  Reset filters │
│  ▾ Pinned    ├───────────────────────────────────────────────────────────────────────────────────────────┤
│    api.x.com │ ● ID  URL                               Client    Method Status    Code Time         Duration Req  Res │
│ All          │ ● 54  https://api.example.com/v1/users  ◉ curl    GET    Completed 200  21:17:50.480 255 ms   0 B  1 KB │
│  ▸ Apps   22 │ …  dense virtual rows (≈24 px)                                                                       │
│  ▸ Domains 41├───────────────────────────────────────────────────────────────────────────────────────────┤
│              │ [GET] [200 OK]  https://api.example.com/v1/users?page=1  (wrapped, monospace)               │
│              ├ Request  Header Query Body Cookies Auth Raw Summary ┬ Response  Header Body Set-Cookie Raw … JSON ▾ ┤
│              │  key/value tables, body viewer                     │  line-numbered, foldable JSON viewer        │
│ ⌕ Filter ⌘⇧F │ Clear  Filter  Auto Select        3/33 rows selected        12 MB · ↑ 3 KB/s ↓ 2 KB/s · Manual │
└──────────────┴───────────────────────────────────────────────────────────────────────────────────────────┘
```

### Toolbar and proxy setup

- Left: Pause/Resume capture, **Compose** (opens Edit and resend with an empty
  request), and Clear.
- Centre: a status pill showing a coloured dot and "Capturing · <address>",
  "Paused" or "Stopped".
  - Clicking it opens a **Proxy setup** popover holding everything that
    currently fills the header: Start/Stop, address, token, routing lines with
    copy buttons, fixed port, Allow local destinations, and the Certificate and
    hosts section. The window header no longer shows these.
- Right: toggle sidebar; detail pane below or to the right; hide detail pane.

### Content-type tabs

A tab strip filters by kind: **All, HTTP, HTTPS, WebSocket** | **JSON, Form, XML,
JS, CSS, GraphQL, Document, Media, Other**, with **Reset filters**.

Classification:
- **Media:** image, audio, video and font types.
- **Document:** HTML.
- **GraphQL:** the path ends in `/graphql`, the content type is
  `application/graphql`, or a JSON body has a top-level `query` string.
- **Tunnels:** under HTTPS and Other.

Kinds are computed once per flow in the store, not per frame. The existing
text, process, host, method, status and error filters move into a filter bar
toggled by **Filter** (⌘F) in the bottom bar.

### Sidebar

- **Favorites.**
  - **Pinned:** apps and domains you pin, from the context menu or a pin
    button. Saved in the GUI `settings.json` with no traffic data, just as the
    decrypt list is.
  - **Saved:** phase 3B rules, with enable toggles and touched-flow counts.
- **All.**
  - **Apps:** each app expands to the domains it contacted, with counts.
  - **Domains:** each registrable domain expands to its hosts.
- A filter field at the bottom (⌘⇧F) narrows the tree.
- Counts stay incremental, as in 3A.

### Flow table

- Dense rows of about 24 px, with alternating tints.
- The selection uses the theme's accent.
- Columns, each sortable and with a visibility menu:
  - Status dot: green complete, amber active, red failed, grey for tunnels and
    refused requests.
  - ID.
  - **URL**, in full, with masked query values.
  - Client (icon + app name).
  - Method.
  - Status (Active, Completed, Failed, Refused, Tunnel).
  - Code.
  - Time (`HH:MM:SS.mmm`).
  - Duration.
  - Request size, response size.
- The context menu offers:
  - Copy URL, Copy as cURL.
  - Repeat (⌘R), Edit and resend (⌘E).
  - Pin app, Pin domain.
  - Decrypt this host (tunnels).
  - Export HAR.

### Detail

- A header line with a method badge and a status badge, coloured by status
  class, then the full URL in monospace, wrapped.
- **Request and Response side by side** in a resizable split. Each side has
  its own tabs:
  - **Request:** Header, Query (decoded key/value table), Body, Cookies
    (parsed), Auth (scheme, value masked through the shared policy), Raw
    (HTTP/1.1 text, redacted), Summary (process, connection, timing,
    attribution wait, sizes).
  - **Response:** Header, Body, Set-Cookie (parsed attributes), Raw; plus
    Messages for WebSockets and Timing.
- **Body viewer.**
  - A format picker offers Auto, JSON, Text, Form, Hex and Image.
  - JSON appears as a line-numbered, foldable tree, syntax-coloured from theme
    roles.
  - Forms appear as key/value tables.
  - Images get a bounded preview.
- All viewers are built off the UI thread from the existing 256 KB preview and
  rendered virtualized. Raw and copy actions keep using the retained bytes.

### Bottom bar

- **Clear**, **Filter**, and **Auto Select** (keep the newest flow selected).
- "x/y rows selected".
- Capture memory against the 64 MB budget, and ↑/↓ throughput over the last
  second.
- A routing chip: "Manual" now; "System proxy" once system-wide capture exists.

### Shortcuts

| Shortcut | Action |
| --- | --- |
| ⌘K | Clear |
| ⌘F | Filter bar |
| ⌘⇧F | Sidebar filter |
| ⌘R | Repeat |
| ⌘E | Edit and resend |
| ⌘⇧C | Copy as cURL |
| ⌘. | Pause |
| ↑ / ↓ | Move the selection |
| Esc | Clear the filters |

### Implementation notes

- **Classification.** The store keeps a classifier per flow and updates the
  row's kind when the request head, the complete request body (once, for
  the GraphQL `query` check) and the response head arrive. A form request
  stays Form whatever it returns; otherwise the response type wins, then the
  request type. Tunnels and upgrades are Other. Rows also carry the masked
  URL and a WebSocket flag; the CLI's JSON skips all three.
- **Tabs.** One transport (All, HTTP, HTTPS, WebSocket) and at most one
  content kind combine; clicking a selected kind clears it.
- **Pins.** Apps pin by name (attributed apps only), domains by registrable
  domain. `settings.json` keeps at most 200 validated names of each.
- **Auth tab.** Details expose the `Authorization` scheme without its
  credentials. A lone token is treated as the credential, never a scheme.
- **Compose.** `Session::compose` sends a request built from scratch
  through the replay path, so destination policy, the decrypt list and TLS
  verification apply as for Repeat.
- **Layout.** With details on the right, the request and response panes
  stack vertically. Column visibility and pane sizes last for the window.
- **Selection.** Single selection only; "x/y rows selected" counts the
  selected flow when it is visible.

## Phase 3B: live editing

Phase 3B adds four tools that change traffic as it passes through Zentop:
breakpoints, Map Local, Map Remote and rewrite rules. They share one rules
model and run only in the GUI. The CLI never runs rules.

### Rules model

A rule has a stable numeric ID, a name, an enabled flag, a match and an action.

- **Method:** one method token such as `POST`, or any method.
- **URL pattern:** `*` matches any run of characters, including `/`. Nothing
  else is special. Patterns match `scheme://host[:port]/path?query`.
  - The scheme and host are compared case-insensitively. The path and query
    are compared exactly.
  - The default port (80 for `http`, 443 for `https`) is left out, as browsers
    show it.
  - A pattern without `://` matches any scheme. A pattern without `/` after
    the host matches any path. A pattern without `?` ignores the query.
- **App (optional):** the attributed process name, compared without regard to
  case. Unknown processes never match a rule that names an app.

**Scope.**
- Rules apply only to traffic Zentop sees in plaintext: plain HTTP requests
  and requests inside decrypted HTTPS connections.
- They never apply to CONNECT tunnels, HTTP/1.1 upgrade handshakes
  (WebSockets), Repeat or Compose.
- Rules are evaluated after the decryption decision. They never add hosts to
  the decrypt list or bypass the skip list.
- Pausing capture keeps rules other than breakpoints in effect. Pausing only
  suspends recording.

**Order.** Enabled rules are considered in list order.
1. **Destination.** The first matching Map Local or Map Remote rule decides
   where the request goes. Later mapping rules are ignored.
2. **Rewrite.** Every matching rewrite rule applies in list order: request
   actions before the request is sent, response actions when the response
   head arrives.
3. **Breakpoint.** A matching breakpoint holds the request after its rewrites,
   and the response after its rewrites.

Map Local sends nothing upstream, so request rewrites and request breakpoints
are skipped. The local response still passes through response rewrites and
response breakpoints.

**Marks.** The store records each rule that touched a flow, with its kind and a
short note, such as "Held 4.2 s, edited" or "Body skipped: not text".
- Rows carry a compact list of the rule IDs and kinds. The URL column shows a
  small badge per kind (breakpoint, Map Local, Map Remote, rewrite).
- The request Summary gains a **Rules** line listing each rule and its note.
- The recorded request and response are what Zentop actually sent: the
  request after mapping, rewrites and breakpoint edits, and the response the
  client received.

### Breakpoints

A breakpoint rule holds requests, responses or both. **⌘B** toggles a master
switch in the toolbar. While it is off, breakpoint rules match nothing.

- **Holding.** A held message appears in the **Breakpoints** window, which
  comes forward when something is held. Held messages are listed newest first,
  each with a countdown to its timeout.
- **Editing.** The editor extends Edit and resend.
  - Requests: method, URL, headers and body.
  - Responses: status, headers and body.
  - Secret values are masked by the shared redaction policy. Untouched masked
    values keep their original values, as in Edit and resend.
- **Execute** sends the edited message.
  - An edited URL goes through destination policy again. An `https://` URL is
    fetched with platform-verified TLS.
- **Abort** answers the client with `502` and the body "Zentop breakpoint
  aborted this request" or "…response".
  - An aborted response is replaced, never forwarded. Its head has not reached
    the client while it is held, so a `502` is still possible.
- **Timeout.** Holds time out after 2 minutes. The message then continues
  unmodified, and the flow records the timeout.
- **Cap.** At most 32 messages are held at once. Beyond that, matching messages
  pass through unmodified, and a counter records each one.
- **Bodies.**
  - A body of up to 1 MB is read before the hold, charged to the shared 64 MB
    budget while held, and is editable.
  - A body that is larger, or doesn't fit the budget, isn't editable. Execute
    sends any head edits with the original body streamed after them.
  - A `Content-Length` over 1 MB is never read before the hold.
- **Session controls.**
  - **Pause** and the master switch release every held message unmodified,
    and suspend new holds.
  - **Stop** closes held connections with the rest of the session.
  - **Clear** leaves holds alone.
- **Concurrency.** A hold waits in its own connection's task, or its own
  HTTP/2 stream. Other connections and streams keep flowing.

### Map Local

A Map Local rule answers from a file without contacting the origin.

- **Settings.** An absolute file path, a status (200–599, default 200), an optional
  content type, and up to 32 extra headers.
  - The content type defaults to a guess from the extension, or
    `application/octet-stream`.
- **Reading.** The file is read at request time on a blocking worker, never
  the runtime thread.
- **Limits.** Only regular files of at most 10 MB are served.
- **Failures.** A missing file, a directory or other non-regular file, an
  oversized file or a read error answers `502` with the reason. The flow
  records the reason as its failure.
- **Request body.** The request body is drained and recorded within the normal
  capture limits.
- **Scope.** Directory mapping is out of scope for this phase.

### Map Remote

A Map Remote rule changes the destination before Zentop dials. It can change
the scheme, host, port and path, and can keep or drop the query.

- **Port.** A blank port keeps the original port when the scheme and host are
  unchanged. Otherwise it uses the scheme's default port.
- **Destination policy.** The new destination goes through the same policy as
  any request. A local or private address needs **Allow local destinations**.
- **TLS.** An `https://` destination is fetched with platform-verified TLS
  (`rustls-platform-verifier`), with hostname checks.
  - Mapping plain HTTP to HTTPS is allowed.
  - Mapping decrypted HTTPS to plain HTTP is allowed.
  - Verification is never relaxed. Tests inject a verifier that trusts a test
    CA, as phase 2 does.
- **Host header.** Zentop sets `Host` (or `:authority`) to the new destination.

### Rewrite

A rewrite rule holds up to 32 actions, each applied to the request or the
response:

- **Headers:** add, replace (set, removing other values), or remove.
- **Status:** set the response status code (200–599).
- **Body:** replace text with a literal or regex find and replace. Regex
  replacements can use `$1` capture groups.
  - Bodies are rewritten only when they are at most 1 MB and valid UTF-8 text.
  - `gzip`, `deflate` and `br` bodies are decoded to at most 1 MB and then
    sent uncompressed, without `Content-Encoding`.
  - A rewritten body over 4 MB is discarded.
  - A body that can't be rewritten is passed through unchanged, and the flow
    notes why.
- **Query parameters** (request only): add or replace a parameter, or remove
  one.

**Framing.** After a body change, Zentop removes `Content-Length` and
`Transfer-Encoding`. Hyper then frames the new body with an exact length, on
both HTTP/1.1 and HTTP/2.

### Persistence

- Rules are saved in GUI `settings.json` under `behavior.trafficRules`.
  Captured traffic is never saved there.
- At most 200 rules are kept. Each one is validated when loaded and when
  saved, and invalid rules are dropped.
- Limits per field:
  - name: 100 characters;
  - URL pattern: 2 KB;
  - app name: 255 characters;
  - header values: 8 KB;
  - regexes: 1 KB, compiled with a 256 KB program limit and a 1 MB DFA limit.
- Map Local stores only the file path. Rule IDs are unique, and the GUI
  assigns the next free ID.
- The breakpoint master switch lasts for the app session and starts on.
- Importing and exporting rule files is left for later.

### Interface

- **Toolbar.**
  - **Rules** opens the Rules window.
  - The breakpoint switch (⌘B) shows its state.
  - **Breakpoints** (⌘⇧B) opens the queue and shows a count while anything is
    held.
- **Rules window.**
  - A list of every rule with its kind, enable checkbox, move up and down,
    and delete.
  - An editor for the selected rule: name, method, URL pattern, app and the
    action's fields.
  - **Save** validates the rule through the proxy crate, then persists it and
    applies it to the running session.
- **Row context menu.**
  - **Breakpoint this URL** creates and saves an enabled request-and-response
    breakpoint for the row's URL without its query.
  - **Map Local…**, **Map Remote…** and **Rewrite…** open the Rules window
    with an unsaved draft prefilled from the flow.
- **Favorites → Saved.**
  - Lists rules with enable toggles and the number of retained flows each one
    touched.
  - Clicking a rule filters the table to those flows.
- **Breakpoints window.**
  - The held list and the editor, with Execute and Abort.
  - It comes forward whenever a new message is held, even if the Traffic
    window is closed.

### Security

- Rules are local and written by you. Nothing is fetched or shared.
- Regexes have size limits and run only on bounded bodies.
- Bodies are never logged.
- Map Local reads only regular files you named, at request time, under a size
  cap.
- Map Remote and edited URLs go through destination policy and verified TLS.
- Rules cannot widen decryption.
- The CLI does not load or run rules.

### Testing

- **Unit:**
  - wildcard matching;
  - rule validation and the settings cap;
  - each rewrite action, including `Content-Length` recomputation and
    compressed bodies;
  - Map Local file checks: missing, too large, not a regular file;
  - Map Remote destination building;
  - the breakpoint queue: hold, edit and execute, abort, timeout, cap,
    release;
  - store marks.
- **Integration** (`tests/session.rs`), driving real HTTP through the proxy:
  - a request breakpoint edited and executed;
  - a response breakpoint aborted;
  - a timeout;
  - other connections flowing while one is held;
  - Map Local;
  - Map Remote to a second origin, refused without Allow local destinations,
    and to HTTPS verified with the injected test verifier;
  - rewrites;
  - a CONNECT tunnel that matches a rule but stays untouched.

## Phase 3D: Open with Zentop

Desktop apps built on Chromium (Google Chrome and other Chromium browsers,
and Electron apps such as Claude, Slack, VS Code, Discord, Notion and
Cursor) ignore `http_proxy` and can't send the session token. Phase 3D lets
Zentop start such an app pointed at its proxy, and authorizes the app by
process family instead of by token. The Mac's network and proxy settings
never change. The GUI only; the CLI is unchanged.

### Launch

Zentop starts the chosen app with Chromium's proxy switch, which Chromium
browsers and Electron apps honour:

```
--proxy-server=http://127.0.0.1:<port>   # no credentials in the URL
--disable-quic                           # QUIC is never proxied
```

- **Launching.** `NSWorkspace openApplicationAtURL:configuration:` with
  `arguments` and `createsNewApplicationInstance`. Its completion handler
  returns the `NSRunningApplication`, whose PID Zentop records. LaunchServices
  starts the app, so macOS holds the app itself responsible for its helpers,
  not Zentop.
- **Chromium browsers** get a fresh temporary profile, so the user's normal
  browser session and its running instance are untouched:
  `--user-data-dir=<temp> --no-first-run --no-default-browser-check`.
  - Profiles live under `$TMPDIR/zentop-launched-profiles/<zentop pid>-<n>`.
  - A profile is deleted when its browser exits, or when capture stops
    after the browser has exited.
  - On the next start, Zentop deletes profiles whose Zentop and browser
    (from the profile's `SingletonLock`) are both gone. That covers a Zentop
    crash.
- **Electron apps** keep their normal data, so they stay signed in. Electron
  apps are usually single-instance: a second launch hands off to the
  running instance and exits.
  - If the app is already running, Zentop asks first. It never quits an app
    on its own.
  - Only after explicit confirmation does Zentop quit it, gracefully with
    `NSRunningApplication terminate` (the app may still ask to save work),
    never with a signal. Zentop waits up to 10 seconds for it to exit, then
    launches. If it is still running, Zentop says so and launches nothing.
- **Arguments** are passed as an argument array, never through a shell.

### App discovery

- Zentop lists `.app` bundles in `/Applications` and `~/Applications`,
  including one level of subfolders, reading each `Info.plist`.
- **Chromium browsers** by bundle ID: Google Chrome (stable, beta, dev and
  Canary), Chromium, Brave, Microsoft Edge, Vivaldi, Opera and Opera GX.
  - Arc is left out: it isn't installed on the development Mac, so its
    handling of `--proxy-server` is unverified.
- **Electron apps** by `Contents/Frameworks/Electron Framework.framework`.
- The menu shows each app's icon and name, browsers first. Zentop itself is
  excluded.
- **Choose app…** accepts any `.app`. An app that is neither kind gets a
  warning that it may ignore the proxy switch, with Continue and Cancel.
- Discovery runs off the UI thread when the Traffic window opens.

### Client-access policy

A connection is authorized if either:

1. it sends the session token, unchanged from phase 1; or
2. attribution resolves it to a process in a **launched family**.

A launched family belongs to one app Zentop launched in this capture session.
It is keyed by the root process's macOS **unique process ID** (`p_uniqueid`,
64-bit, never reused while the Mac is up), not by PID. A process is in the
family if its executable lies inside the launched app's bundle and it is:

- the launched process itself (same unique ID);
- a descendant: its parent's unique ID, or an ancestor's, is the root's.
  Each step up compares the recorded parent unique ID
  (`PROC_PIDT_BSDINFOWITHUNIQID`) with the parent's actual unique ID, so a
  reused PID breaks the chain instead of joining it; or
- a helper macOS holds the root responsible for
  (`responsibility_get_pid_responsible_for_pid`, as in the process list).
  The responsible PID's current unique ID must be the root's, and the root
  must have started no later than the helper. A process can't be
  responsible for one that started before it, so a reused PID can't pass.

The decision happens after the existing first-request attribution wait (up
to 250 ms).
- Attribution that is unknown or ambiguous never authorizes.
- The attributed PID must still have the attributed unique ID when the
  family is checked.
- The result is cached per connection, but every request checks that the
  family is still registered.
- Unauthorized requests get `407`, as before, and are not recorded.
- **Launch grace.** While a launch is in progress, an unauthorized request
  waits up to 5 seconds for the family to be registered, then is checked
  again. Otherwise Chromium's first connections would race the registration
  and show a proxy sign-in prompt.

Families are registered only for apps Zentop launched in the current
capture session. A family is dropped when you choose **Stop capturing** for
it, when its root exits (its PID no longer has its unique ID), or when
capture stops. Open tunnels finish on their own; new connections from a
dropped family get `407`.

Why this keeps others out:
- **Other local users.** Their processes can't descend from, or be the
  responsibility of, an app launched in your session. Zentop also can't read
  their process details, so their connections stay unattributed.
- **Other apps.** Only the launched app's own processes match, and only
  executables inside its bundle. That excludes, for example, a shell in an
  editor's integrated terminal. That shell still needs the token. PID reuse
  is defeated by unique IDs and start-time order.
- **Web pages.** A page in another browser reaches the port from that
  browser's process, which isn't in a family. A page in the launched browser
  can't make the browser send proxy-form or CONNECT requests. Chromium
  bypasses the proxy for loopback, so a page fetching the proxy port directly
  sends an origin-form request, which gets `400`. The destination policy
  still refuses local and private addresses.
- **Same-user malware** is out of scope, as it is for the token: such a
  process can already launch the app itself or read your traffic in other
  ways.

### HTTPS

Decryption still applies only to exact decrypt-list hosts. Everything else
stays a blind tunnel, where the app verifies the real server itself. There
are two explicit ways for a launched app to accept Zentop's certificates:

- **(a) Trust the root** (the existing Trust button). It works for every app,
  and changes user-domain trust.
- **(b) Accept in the launched instance only.** For launched Chromium and
  Electron apps, Zentop can pass
  `--ignore-certificate-errors-spki-list=<base64 SHA-256 of an SPKI>`. That
  instance then accepts Zentop's certificates with no trust change.
  - It's off by default. It's chosen per launch in the menu, and needs a
    usable certificate.
  - Chrome shows an "unsupported command-line flag" warning bar.

**What the flag matches.** Tested with Chrome 157, headless, with a temporary
profile, against a local TLS server and an untrusted test root:

| Hash in the flag | Chain the server sends | Result |
| --- | --- | --- |
| none | leaf + root | rejected |
| root | leaf + root | accepted |
| root | leaf only | rejected |
| leaf | leaf only | accepted |
| leaf | leaf + root | accepted |

Chrome hashes the certificates the server presents, the leaf and any
intermediates, and accepts the chain if any hash is listed. It doesn't check
that they chain together.

That's a trade-off. Any server that presents a certificate with the listed
key passes in that instance, even through a blind tunnel. A certificate is
public, so the listed key must not be one an attacker could see.
- The long-lived root is a poor choice. It sits unprotected in the login
  keychain, and Proxy setup exports it as a PEM.
- Zentop instead creates one in-memory **session leaf key** when you first
  launch with this option. From then on, every leaf in that capture session
  uses that key. The key is never stored, and is replaced every session.
- The flag lists that key's hash, and leaves are sent without the root, as
  before.
- The cost: for the rest of that session, leaves share one key instead of
  one key per host. The key never leaves memory, and only a local client the
  proxy has authorized ever sees a certificate carrying it.

**`--user-data-dir`.** Chromium documents that the switch has no effect
unless `--user-data-dir` is present. Zentop always passes it to browsers, so
this doesn't matter for them. Testing without it would have used the
developer's own Chrome profile and running Chrome, so it wasn't run. Electron
apps keep their own data directory, so whether they honour the flag is
verified per app. If one doesn't, use option (a).

**Verified live** with Chrome 158 on macOS 26, through Open App with a
temporary profile and an isolated keychain, with no trust change: with the
option on, `https://example.com` on the decrypt list loaded and its rows
were decrypted; with it off, Chrome rejected Zentop's certificate and the row
said so. That confirms the session leaf key (a leaf SPKI) is what Chrome
needs. Claude desktop 2.26454 quits about 100 ms after start when given
`--ignore-certificate-errors-spki-list`, and runs normally with only
`--proxy-server` and `--disable-quic`. So an app that exits within 3 seconds
of launching gets a message, and when the option was on, the message
suggests turning it off.

**Caveats.**
- Apps that pin certificates reject Zentop's leaf whatever you choose, and
  the row says the program rejected Zentop's certificate.
- QUIC (HTTP/3) is UDP and never proxied. Chromium already falls back to TCP
  when a proxy is set, and `--disable-quic` makes that explicit.
- Electron apps that make requests from Node.js, in the main process or in
  extensions, don't use Chromium's proxy switch. Those requests go direct
  and aren't captured.

### Lifecycle and interface

- **Toolbar.** An **Open App with Zentop** menu lists the discovered apps
  with icons, then **Choose app…**, and an **Accept Zentop's certificate in
  launched apps** check item for option (b).
  - If capture is stopped, launching starts it first with the window's
    settings.
- **Proxy setup** gains a **Launched** list. Each row shows the app's icon,
  name, PID and state (Capturing or Not capturing). An app leaves the list
  when it exits. Each row has:
  - **Stop capturing**, which drops the family; and
  - **Quit…**, which asks first and then quits that instance gracefully.
- **Sidebar.** Flows from a launched family carry the launched app's name.
  The Apps tree groups them under it, for example **Google Chrome** with
  its icon. The Client column still shows the helper process that owned the
  socket. Both use the launched app's icon, since a helper that has just
  started may not be in the sampled process table yet.
- **When capture stops** while launched apps still run, a notice says they
  can't reach the network until relaunched normally, with **Quit launched
  apps…** (one confirmation for all).
- Quitting Zentop doesn't quit launched apps. Their profiles are cleaned up
  on the next start.

### Testing

- **Unit:**
  - the launch argument builder: Chrome against Electron arguments, the
    temporary profile, the proxy URL, `--disable-quic`, and the SPKI flag
    only when chosen;
  - app discovery against fixture bundles (browsers by bundle ID, Electron
    by framework, others ignored, Zentop excluded);
  - stale-profile cleanup;
  - family authorization against a fake process table:
    - the launched PID, a child and a responsible helper are authorized;
    - an unrelated PID, unknown attribution, a reused PID with another
      unique ID, a helper outside the bundle, and a responsible process that
      started after the helper are refused;
    - a family is dropped after Stop capturing or after its root exits;
  - session leaf key reuse and its SPKI hash.
- **Integration** (`tests/session.rs`): a child process spawned by the test,
  in a registered family, gets through without a token. An unrelated client
  (the test process) gets `407`. After the family is dropped, the same child
  gets `407`.
- **Live:** launch Chrome with a temporary profile and browse plain and
  decrypted HTTPS. Rows must be attributed to Chrome's helpers with no
  token, and curl without the token must still get `407`.

### Terms of service

Inspecting your own traffic is fine. Replaying or automating requests
against a service's private API with your session (Repeat, Edit and resend,
breakpoints, rules) may breach that service's terms. Use official APIs for
automation, and never capture other people's traffic. The user guide says
this next to Open with Zentop.

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
  Phase 3D's launched-family policy covers only apps Zentop starts itself.
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
- **Launching apps with Chromium's proxy switch, over system-wide capture,
  for browsers and Electron apps (phase 3D).** No system settings change, and
  the launched family is authorized by unique process ID, so the token
  still guards everything else.
- **A session leaf key for the SPKI switch, over the root's key.** Chrome
  accepts any presented certificate whose key is listed, so the listed key
  must never appear in a public certificate.

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
