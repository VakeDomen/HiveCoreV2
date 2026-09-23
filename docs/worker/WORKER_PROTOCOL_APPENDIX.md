# HiveNode Worker Protocol Appendix

## Purpose

This appendix records the wire-level patterns used by HiveNode as implemented in this repository. It is intended as a practical reference for protocol debugging and HiveCore integration work.

## Transport Model

HiveNode maintains a persistent TCP connection to HiveCore.

Inbound messages from HiveCore are read as:

1. a 4-byte big-endian message length
2. exactly that many message bytes

The message bytes are then parsed as a textual request with this shape:

```text
<METHOD> <URI> <PROTOCOL>\r\n
Header-Name: value\r\n
...\r\n
\r\n
<body>
```

## Authentication

Worker authentication is sent by HiveNode as a plain text line:

```text
AUTH <HIVE_KEY>;<nonce>;<node_version>;<ollama_version> HIVE\r\n
```

Example:

```text
AUTH worker-secret;1710595023123;0.1.7;0.6.0 HIVE\r\n
```

Semantics:

- `HIVE_KEY` identifies the worker
- `nonce` is generated once per process start
- `node_version` is the HiveNode build version
- `ollama_version` is discovered from `GET /api/version`

## Polling

HiveNode uses two polling formats.

Optimized polling:

```text
POLL - HIVE\r\n
```

Explicit model-target polling:

```text
POLL <model_target> HIVE\r\n
```

Example:

```text
POLL llama3.2:latest;deepseek-r1:8b HIVE\r\n
```

## Inbound Hive Control Messages

HiveCore sends control messages with `protocol == HIVE`.

Examples:

```text
PONG / HIVE\r\n
\r\n
```

```text
REBOOT / HIVE\r\n
\r\n
```

```text
UPDATE_OLLAMA / HIVE\r\n
\r\n
```

Current recognized worker commands:

- `REBOOT`
- `SHUTDOWN`
- `UPDATE`
- `UPDATE_OLLAMA`

`PONG` is handled as a no-op keepalive.

## Latency Echo (opt-in, backwards compatible)

Nodes may measure their round-trip latency to HiveCore without any shared clock.
The feature is opt-in from the node's side, so older HiveNode builds are
unaffected.

The latency probe uses a **dedicated control method, `LATENCY`**, so it can
never be confused with the `-` / model-name URI convention of the `POLL*`
methods — those semantics apply only to polling frames, never to a `LATENCY`
frame.

To probe, send a `LATENCY` frame whose URI is a token:

```text
LATENCY <echo-token> HIVE\r\n
```

HiveCore immediately replies with the token echoed back (only the bare token,
never anything appended to it):

```text
PONG <echo-token> HIVE\r\n
```

The node times the send-to-receive round trip to obtain the true machine ↔
proxy RTT.

A node may report its most recently measured latencies back to HiveCore by
appending `;`-separated millisecond values to the URI. The report is positional:

```text
LATENCY <echo-token>[;<core-node-ms>[;<node-backend-ms>]] HIVE\r\n
```

- `;12` — one number: the **core↔node** round trip (12 ms), exactly as before.
- `;12;3` — two numbers: the **core↔node** round trip (12 ms) and the
  **node↔backend** round trip (3 ms, the node to its local inference backend).

HiveCore records both and exposes them through the management API
(`GET /worker/pings`):

- `core_to_node_rtt_latency` — the core↔node round trip in ms (`null` when the
  node does not support the opt-in latency echo).
- `node_to_backend_rtt_latency` — the node↔backend round trip in ms (`null`
  when the node reports only the core↔node leg).
- `latency_age_ms` — how long ago (in ms) the report was received (`null` when
  never reported).
- `latency_stale` — `true` when `latency_age_ms` exceeds the configured
  `LATENCY_REPORT_TIMEOUT` (default 30 s), i.e. the stored values should not be
  trusted as current.

The report is carried in the URI because HIVE control frames are parsed
line-oriented (only the request line is read), so headers and bodies are not
available on control frames. HiveCore echoes only `<echo-token>` back — never
the report suffixes — so the node can validate the reply matches exactly what
it sent.

Old nodes never send `LATENCY`, so both latency fields are `null` for them and
`PING` keeps its original pure-keepalive behavior.

## Inbound Proxied HTTP Messages

Non-`HIVE` messages are treated as proxied Ollama requests.

Example generate request:

```http
POST /api/generate HTTP/1.1
Content-Type: application/json

{"model":"llama3.2","prompt":"hello"}
```

Example tags request:

```http
GET /api/tags HTTP/1.1

```

## Forwarding Rules

When HiveNode proxies an HTTP message to Ollama:

- the original method is preserved
- the target becomes `OLLAMA_URL + uri`
- `Host` is dropped
- `Content-Length` is dropped
- the request body is forwarded if present
- a 30-minute timeout is applied

HiveNode refuses to forward messages whose parsed protocol is `HIVE`.

## Outbound HTTP Response Streaming

For proxied Ollama responses, HiveNode writes an HTTP/1.1 response back to HiveCore.

Status line form:

```http
HTTP/1.1 200 OK
```

Header behavior:

- original Ollama headers are forwarded except `Transfer-Encoding`
- `Transfer-Encoding: chunked` is added
- `Connection: close` is added

Body behavior:

- the response body is read incrementally
- each chunk is written using HTTP chunked transfer encoding
- the stream ends with:

```text
0\r\n
\r\n
```

Example chunked body:

```text
B\r\n
hello world\r\n
0\r\n
\r\n
```

## Worker Command Acknowledgements

The current HiveNode implementation writes HTTP-style acknowledgements on the worker stream for recognized control commands.

Examples:

Success:

```http
HTTP/1.1 200 OK
Content-Length: 25
Content-Type: text/plain; charset=utf-8
Connection: close

HiveNode will reconnect.
```

Accepted update:

```http
HTTP/1.1 202 Accepted
Content-Length: 66
Content-Type: text/plain; charset=utf-8
Connection: close

Ollama Docker update started. HiveNode will reconnect when ready.
```

Conflict in external mode:

```http
HTTP/1.1 409 Conflict
Content-Length: 59
Content-Type: text/plain; charset=utf-8
Connection: close

UPDATE_OLLAMA is only available when OLLAMA_MODE=docker.
```

Important note:

The exact compatibility contract between these worker-stream acknowledgements and HiveCore’s `/worker/command` HTTP bridge should be treated as an integration concern. Changes here must be verified against HiveCore’s parser.

## Model Refresh Triggers

HiveNode treats these successful proxied endpoints as model-changing operations:

- `POST /api/pull`
- `DELETE /api/delete`

When either completes successfully, HiveNode signals a refresh so future polls advertise updated model state.

## Parsing Notes

`ProxyMessage` parsing rules:

- the first line is split into method, URI, protocol
- headers are parsed until the first empty line
- all remaining lines become the body joined by `\n`

That means the worker protocol is line-oriented and tolerant of empty bodies, but its correctness depends on HiveCore sending valid framed messages.
