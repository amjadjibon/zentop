# Traffic inspector

On macOS, open **View → Traffic…** or choose **Traffic** in the menu-bar
overview. Click the status pill at the top of the window (**Stopped**,
**Capturing · address** or **Paused**) to open **Proxy setup**, then click
**Start**. Proxy setup shows the loopback address, the session token and
shell commands to route a program through it. To reuse a port, enter it
under **Fixed port** and click **Save port**. A blank field lets macOS select
an available port.

Capture continues when you close the Traffic window. Click the Dock icon to
reopen the main window, then choose **View → Traffic…**. Stop capture from
Proxy setup or the menu-bar overview, or quit Zentop. **Pause** (⌘.) keeps
forwarding and counting traffic while suspending recording. **Clear** (⌘K)
drops completed flows and stops body capture for flows already in progress.
**Stop** closes open connections and frees the capture.

## Route a program

Copy the routing commands from Traffic into a terminal. They apply only to
that shell and the programs it starts.

```sh
export http_proxy=http://zentop:<token>@127.0.0.1:<port>
export https_proxy=$http_proxy
curl -x "$http_proxy" http://example.com/
curl -x "$https_proxy" https://example.com/
```

Replace the placeholders with the displayed token and port. Lowercase
`http_proxy` is required for curl to pick it up from the environment. Use
`unset http_proxy https_proxy` when finished.

In Node.js versions with `--use-env-proxy`, use the same environment variables.

```sh
node --use-env-proxy -e 'fetch("http://example.com/").then(async r => console.log(r.status, await r.text()))'
```

For a Node script using Undici, set an explicit proxy dispatcher.
[Undici ProxyAgent](https://github.com/nodejs/undici/blob/main/docs/docs/api/ProxyAgent.md)

```js
import { fetch, ProxyAgent } from 'undici';

const proxy = new URL(process.env.http_proxy);
const token = Buffer.from(`${proxy.username}:${proxy.password}`).toString('base64');
const dispatcher = new ProxyAgent({ uri: proxy.origin, token: `Basic ${token}` });
try {
  const response = await fetch('https://example.com/', { dispatcher });
  console.log(response.status);
  await response.body.cancel();
} finally {
  await dispatcher.close();
}
```

Other clients can use the displayed proxy host and port with Basic proxy
authentication. Use `zentop` as the username and the session token as the
password. Applications that ignore proxy environment variables need their own
proxy configuration.

## Decrypt HTTPS in the GUI

1. Open **Certificate and hosts** in Proxy setup. Click **Create** to
   generate a root certificate and its non-exportable private key in macOS
   Keychain.
2. Click **Trust** to ask macOS to trust the certificate for SSL in your user
   account. For a single tool, use **Export certificate** and the commands
   below instead.
3. Type an exact DNS hostname in the decrypt list and click **Add host**, or
   choose **Decrypt this host** from a tunnel row's context menu or details. Reconnect the program after
   adding or removing a host. Subdomains need their own entries.
4. Start capture and route the program through Zentop. **Decrypted** rows
   show individual requests and responses, including headers, bodies and TLS
   timing. Unlisted hosts keep using blind CONNECT tunnels.

Zentop verifies each upstream HTTPS connection with the macOS trust store and
hostname checks. It supports HTTP/1.1 and HTTP/2 on both sides of a decrypted
connection. If an upstream handshake fails, open the row to see the error.
If a program rejects Zentop's certificate, the failed CONNECT row reports
the rejection. WebSocket upgrades over HTTP/1.1 work on decrypted hosts.
WebSockets also pass through blind tunnels, where their frames stay opaque.

The built-in skip list always tunnels Apple services such as Apple Account,
software updates, push notifications and iCloud, plus some commonly pinned
services such as Google sign-in, Signal and WhatsApp. Select a skipped
tunnel to see the reason. This list is best-effort and cannot identify
every pinned service.

### Certificate states

| Status | What to do |
| --- | --- |
| No certificate | Click **Create**. |
| Created · not trusted | Click **Trust**, **Export certificate**, or **Remove**. |
| Trusted · expires with a date | Keep using it until expiry, or click **Remove**. |
| Certificate key unavailable | Click **Unlock and retry**. If the build lost access to the old key, click **Replace**. |
| Expires with a date | Click **Replace** when fewer than 30 days remain. Export the new PEM for tools that used the old one. |
| Partly removed | Click **Retry removal**. The panel identifies items that remain and shows their manual cleanup steps. |

If Keychain becomes unavailable during capture, decryption turns off for the
session and CONNECT tunnels still work. Restore access and restart capture
to resume decryption. **Replace** creates and trusts a new root before
removing the previous root. Cancelling macOS trust prompts leaves the
previous certificate in place.

### Trust one tool with the exported PEM

**Export certificate** saves the public certificate as PEM. Use the actual
export path below. The app shows copyable commands with that path after export.

```sh
curl -x "$http_proxy" --cacert /path/to/zentop-ca.pem https://example.com/
NODE_EXTRA_CA_CERTS=/path/to/zentop-ca.pem node --use-env-proxy -e 'require("node:https").get("https://example.com/", r => { console.log(r.statusCode); r.resume(); })'
export SSL_CERT_FILE=/path/to/zentop-ca.pem
export REQUESTS_CA_BUNDLE=/path/to/zentop-ca.pem
```

`NODE_EXTRA_CA_CERTS` extends Node's trusted roots for the new process.
`SSL_CERT_FILE` applies to compatible OpenSSL clients, and
`REQUESTS_CA_BUNDLE` applies to Python Requests. Both bundle variables can
replace the tool's default bundle. Scope them to the command or shell you
use for capture.

### Remove the certificate and stop routing

Stop capture, remove the hosts from the decrypt list, and click **Remove**
in **Certificate and hosts**. Zentop checks the user trust setting, the
certificate and the private key separately. If an item remains, use the
commands or Keychain Access steps shown for that root's SHA-256 fingerprint.
If you need to remove it manually, export a copy of the PEM before removing
the certificate:

```sh
security remove-trusted-cert /path/to/zentop-ca.pem
security delete-certificate -Z <SHA-256 fingerprint without colons> "$HOME/Library/Keychains/login.keychain-db"
```

Use the key label shown by Zentop to remove a remaining key in Keychain
Access. To locate the certificate in Keychain Access, search for
**Zentop Local Proxy CA** and compare the SHA-256 fingerprint.
Delete your exported PEM and unset any proxy or certificate environment
variables you set for the capture.

## Read a capture

Plain HTTP rows show the request, response and timing. Cleartext HTTP/2
clients can connect using prior knowledge. Zentop forwards plain HTTP over
HTTP/1.1 to the origin. HTTP/1.1 upgrades such as WebSockets show
the handshake, timing and byte counts. The **Messages** tab shows WebSocket
frames in both directions, including text, binary hex, ping, pong and close
codes. A **Tunnel** row shows the destination, byte counts and timing of a
CONNECT tunnel whose TLS goes directly to the server. Process
attribution is best effort and may name a helper process.
Short-lived connections, shared sockets and sockets Zentop cannot inspect may
show **Unknown process**.

The tab strip filters by transport (**All**, **HTTP**, **HTTPS**,
**WebSocket**) and content (**JSON**, **Form**, **XML**, **JS**, **CSS**,
**GraphQL**, **Document**, **Media**, **Other**). GraphQL covers `/graphql`
paths, `application/graphql` bodies and JSON bodies with a top-level `query`
string. Tunnels appear under HTTPS and Other.

The sidebar lists **Apps**, each expanding to the domains it contacted, and
**Domains**, each expanding to its hosts. Counts update live, and hosts
sharing a registrable domain group together. Pin an app or domain from its
context menu, or the pin button that appears on hover, to keep it under
**Favorites**. Pins are saved in the GUI settings as names only. The field at
the bottom of the sidebar (⇧⌘F) narrows the tree.

**Filter** (⌘F) in the bottom bar shows search and filters for app, host,
method, status class and errors. **Reset filters** or Esc clears every
filter and the sidebar selection. Click a column heading to sort; the column
button at the end of the header, or right-clicking it, shows or hides
columns. Use Up and Down to move the selection. **Auto Select** keeps the
newest matching flow selected. The bottom bar shows the selected and visible
row counts, capture memory against the 64 MB budget and throughput over the
last second.

Details show the method, status and full URL, then **Request** and
**Response** side by side. Request tabs show headers, the decoded query,
the body, parsed cookies, the authorization scheme, raw HTTP text and a
summary. Response tabs show headers, the body, parsed Set-Cookie
attributes, raw HTTP text, WebSocket messages and timing. The body viewer
picks a format automatically, or choose JSON (a foldable tree), Text, Form,
Hex or Image. Use the toolbar buttons to place details below or beside the
table, or hide them, and drag the dividers to resize. Open a process's
details from the request **Summary**, or use **Show traffic** from the
process details sheet.

Right-click a row for Copy URL, Copy as cURL (⇧⌘C), Repeat (⌘R), Edit and
resend (⌘E), pinning, Decrypt this host, the rule tools below and Export HAR.

**Repeat** sends the original retained HTTP or decrypted HTTPS request.
**Edit and resend** opens a request editor with the method, URL, headers and
body. **Compose** in the toolbar opens the same editor with an empty request,
which goes through the same destination and decrypt-list checks. Untouched masked values are sent with their original values. Replay
uses the current destination policy, and HTTPS requires the host to remain
on the decrypt list. Responses appear in new **Repeated** rows from Zentop.
Tunnels, upgrades and incomplete captured requests are unavailable for replay.

Use **Export HAR…** from a row's context menu, or **Export filtered HAR…** in
the filter bar, to save a HAR 1.2 file.
Secret headers and query values are masked. **Include bodies** is off by
default. Review any HAR file before sharing it.

Local, private and link-local destinations are blocked by default.
**Allow local destinations** enables routes to development servers for the
current session. The proxy always refuses to connect to itself.

Capture keeps up to 2,000 flows, 64 KB of headers per message and 1 MB per body.
The shared 64 MB capture budget includes WebSocket frames and evicts older
bodies first. Each WebSocket connection retains up to 1,000 frames and 1 MB
of payload while counting additional frames. Payloads may contain secrets. The decoded preview
is limited to 256 KB. The inspector accepts up to 256 simultaneous client
connections. Unauthenticated clients have 10 seconds to authenticate, and
authenticated connections close after 60 seconds without an active request.
Open CONNECT tunnels and upgraded connections can stay idle. The dropped
flow count tracks flows evicted from the capture.

## Edit traffic live

Rules change plain HTTP and decrypted HTTPS traffic as it passes through
Zentop. They never touch CONNECT tunnels, WebSocket upgrades, Repeat or
Compose, and they never change which hosts are decrypted. The CLI doesn't run
rules.

Click **Rules** in the toolbar to open the Rules window. Choose **New** to
create a rule, or right-click a row and choose **Breakpoint this URL**,
**Map Local…**, **Map Remote…** or **Rewrite…** to start from that flow.
**Breakpoint this URL** saves a request and response breakpoint at once. The
other choices open an unsaved draft. Click **Save** to validate the rule,
store it in the GUI settings and apply it to the running capture. Use the
checkboxes, the arrows and **Delete** to enable, reorder and remove rules.

Every rule matches a method (blank for any), a URL pattern and, optionally, an
app name. In a pattern, `*` matches any characters, including `/`.

| Pattern | Matches |
| --- | --- |
| `https://api.example.com/v1/*` | Every path under `/v1/` on that host over HTTPS. |
| `*.example.com/*` | Every subdomain of `example.com` over either scheme. |
| `http://localhost:8080/search?q=*` | `/search` with any `q` value. |
| `api.example.com` | Every request to that host. |

The query is ignored unless the pattern contains `?`, and the default ports 80
and 443 are left out. Rules apply in list order. The first matching Map Local
or Map Remote rule picks the destination. Every matching rewrite rule applies
next, then breakpoints.

- **Breakpoint** holds matching requests, responses or both.
  - Held messages open in the **Breakpoints** window (⇧⌘B), which comes
    forward when something is held. In the editor, change the method, URL,
    status, headers or body, then click **Execute**. **Abort** answers the
    program with `502`.
  - A held message continues unmodified after 2 minutes. Up to 32 messages
    can be held at once, and more pass straight through.
  - Bodies up to 1 MB can be edited. Larger bodies are sent unchanged.
  - **⌘B**, or the breakpoint button in the toolbar, turns every breakpoint
    off or on. Turning breakpoints off, or pausing capture, releases held
    messages unmodified.
- **Map Local** answers from a file without contacting the server. Choose a
  regular file of up to 10 MB, a status, an optional content type and extra
  headers. The file is read when a request matches. If it's missing or too
  large, the program gets `502` with the reason.
- **Map Remote** sends requests to another scheme, host, port or path, with or
  without the query. The new destination follows the same rules as any
  request: local addresses need **Allow local destinations**, and HTTPS
  servers are verified with the macOS trust store.
- **Rewrite** adds, replaces or removes headers, sets the response status,
  sets or removes query parameters, and replaces body text literally or with
  a regular expression (`$1` inserts a capture group).
  - Bodies are rewritten only when they're text of up to 1 MB.
  - Compressed bodies are decoded and sent uncompressed.
  - Zentop recomputes `Content-Length`.

Rows touched by a rule show a small icon for each kind. The request
**Summary** lists each rule and what it did, such as "Held 4.2 s, executed
with edits". **Favorites → Saved** in the sidebar lists the rules, with
checkboxes to turn them on or off and the number of flows each one touched.
Click a rule there to show only those flows.

## CLI

```sh
zentop-cli traffic
zentop-cli traffic --port 9090
zentop-cli traffic --json
zentop-cli traffic --headers
zentop-cli traffic --allow-local
```

The address, token and routing lines go to standard error. Standard output
contains one entry per finished flow, with metadata only by default. Query
strings and bodies are excluded. JSON entries include the flow kind, state
and numeric timings in milliseconds under the `timing` field. **Ctrl+C**
ends the session and prints the capture counters to standard error. Pass
**--allow-local** to route to local development servers during the CLI session.
The CLI always tunnels HTTPS.

## Privacy

Capture is off by default. When enabled, Zentop resolves host names and forwards
requests for the programs you route through it. It listens on IPv4 loopback
with a fresh random token each session. Proxy routing is configured in the
client. User-domain SSL trust changes only when you approve a certificate
Trust, Replace or Remove action.

The token and flows are kept in memory. GUI settings save the optional
fixed port, the exact-host decrypt list, pins and rules. Rules hold no
captured traffic, and Map Local saves only the file path. Zentop keeps the root key in
Keychain and records public certificate data separately to enable recovery
after an interrupted certificate change. Captured data leaves Zentop through the copy controls, replay, HAR exports
or CLI output. Proxy credentials are consumed before capture. Authorization, Cookie,
Set-Cookie, secret-looking headers such as `X-API-Key`, and secret-looking
query values are masked by default. Use **Reveal secrets** to show them for the
selected flow. Copy as cURL omits those values unless you choose **Include
secrets in cURL**. The body copy control copies the retained bytes as
text or base64. Review bodies and custom headers before sharing.
