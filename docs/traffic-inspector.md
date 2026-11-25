# Traffic inspector

On macOS, open **View → Traffic…** or choose **Traffic** in the menu-bar
overview. Click **Start**. Zentop shows its loopback address, the session token
and shell commands to route a program through it. To reuse a port, enter it
under **Fixed port** and click **Save port**. A blank field lets macOS select
an available port.

Capture continues when you close the Traffic window. Click the Dock icon to
reopen the main window, then choose **View → Traffic…**. Stop capture from
the Traffic window or menu-bar overview, or quit Zentop. **Pause** keeps
forwarding and counting traffic while suspending recording. **Clear** drops
completed flows and stops body capture for flows already in progress.
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

1. Open **Certificate and hosts** in the Traffic window. Click **Create** to
   generate a root certificate and its non-exportable private key in macOS
   Keychain.
2. Click **Trust** to ask macOS to trust the certificate for SSL in your user
   account. For a single tool, use **Export certificate** and the commands
   below instead.
3. Type an exact DNS hostname in the decrypt list and click **Add host**, or
   click **Decrypt this host** on a CONNECT row. Reconnect the program after
   adding or removing a host. Subdomains need their own entries.
4. Start capture and route the program through Zentop. **Decrypted** rows
   show individual requests and responses, including headers, bodies and TLS
   timing. Unlisted hosts keep using blind CONNECT tunnels.

Zentop verifies each upstream HTTPS connection with the macOS trust store and
hostname checks. It supports HTTP/1.1 and HTTP/2 on both sides of a decrypted
connection. If an upstream handshake fails, open the row to see the error.
If a program rejects Zentop's certificate, the failed CONNECT row reports
the rejection. WebSockets need a blind tunnel. Remove the host from the
decrypt list and reconnect to use WebSockets there.

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
the handshake, timing and byte counts. Frame contents are skipped. A
**Not decrypted** CONNECT row shows the destination, byte counts and
timing of a tunnel whose TLS goes directly to the server. Process
attribution is best effort and may name a helper process.
Short-lived connections, shared sockets and sockets Zentop cannot inspect may
show **Unknown process**.

Use search or filter by process, host, method, status class, kind or errors.
Select a row for Request, Response and Timing. Click a process name to open
its details, or use **Show traffic** from the process details sheet.

Local, private and link-local destinations are blocked by default.
**Allow local destinations** enables routes to development servers for the
current session. The proxy always refuses to connect to itself.

Capture keeps up to 2,000 flows, 64 KB of headers per message and 1 MB per body.
The shared 64 MB capture budget evicts older bodies first. The decoded preview
is limited to 256 KB. The inspector accepts up to 256 simultaneous client
connections. Unauthenticated clients have 10 seconds to authenticate, and
authenticated connections close after 60 seconds without an active request.
Open CONNECT tunnels and upgraded connections can stay idle. The dropped
flow count tracks flows evicted from the capture.

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
fixed port and exact-host decrypt list. Zentop keeps the root key in
Keychain and records public certificate data separately to enable recovery
after an interrupted certificate change. Captured data leaves Zentop through the copy controls or CLI
output. Proxy credentials are consumed before capture. Authorization, Cookie,
Set-Cookie, secret-looking headers such as `X-API-Key`, and secret-looking
query values are masked by default. Use **Reveal secrets** to show them for the
selected flow. Copy as cURL omits those values unless you choose **Include
secrets in cURL**. The body copy control copies the retained bytes as
text or base64. Review bodies and custom headers before sharing.
