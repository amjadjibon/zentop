# Traffic inspector

On macOS, open **View → Traffic…** or choose **Traffic** in the menu-bar
overview. Click **Start**. Zentop shows its loopback address, the session token
and shell commands to route a program through it. To reuse a port, enter it
under **Fixed port** and click **Save port**. A blank field lets macOS select
an available port.

Capture continues when you close the Traffic window. Stop it from the window
or menu-bar overview, or quit Zentop. **Pause** keeps forwarding and counting
traffic while suspending recording. **Clear** drops completed flows and stops
body capture for flows already in progress. **Stop** closes open connections
and frees the capture.

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

## Read a capture

Plain HTTP rows show the request, response and timing. Cleartext HTTP/2
forwarding uses prior knowledge and requires an HTTP/2 origin. A CONNECT row shows its
destination, byte counts and timing with **Not decrypted**. The client performs
TLS directly with the server, so Zentop cannot see the encrypted URLs, headers
or bodies. Process attribution is best effort and may name a helper process.
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
connections and counts refusals and dropped updates.

## CLI

```sh
zentop-cli traffic
zentop-cli traffic --port 9090
zentop-cli traffic --json
zentop-cli traffic --headers
```

The address, token and routing lines go to standard error. Standard output
contains one entry per finished flow, with metadata only by default. Query
strings and bodies are excluded. **Ctrl+C** ends the session.

## Privacy

Capture is off by default. When enabled, Zentop resolves host names and forwards
requests for the programs you route through it. It listens on IPv4 loopback
with a fresh random token each session. It does not change system proxy or
trust settings.

The token and flows are kept in memory. Only the optional fixed port is saved
to GUI settings. Captured data leaves Zentop through the copy controls or CLI
output. Proxy credentials are consumed before capture. Authorization, Cookie,
Set-Cookie and secret-looking query values are masked by default. You can
reveal them for the selected flow. Copy as cURL omits those values unless you
choose **Include secrets**. The body copy control copies the retained bytes as
text or base64. Review bodies and custom headers before sharing.
