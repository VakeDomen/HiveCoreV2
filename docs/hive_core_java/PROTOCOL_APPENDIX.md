# Protocol Appendix

## Transport Model

HiveCore uses three separate TCP listeners:

- proxy listener for inbound client HTTP-like requests
- worker listener for HIVE control messages plus proxied request forwarding
- management listener for admin HTTP-like requests

Client and management traffic is parsed directly from the socket as text request lines, headers, and optional fixed-length bodies. Worker control traffic uses the same `Request` parser but with protocol token `HIVE`, which means only the request line is consumed.

## Framing Rules

### Client and Management Inbound Framing

Inbound parsing expects:

```text
METHOD URI PROTOCOL\r\n
header-name: value\r\n
...\r\n
\r\n
<optional body if content-length is present>
```

Rules implemented in `src/main/java/upr/famnit/components/Request.java`:

- request line must have exactly three space-separated tokens
- headers are lowercased on read
- body is read only if `content-length` exists
- inbound chunked request bodies are not supported

### Worker Control Framing

Inbound worker control messages are single-line HIVE requests:

```text
AUTH key;nonce;hiveVersion;ollamaVersion HIVE\r\n
POLL llama3.2;phi4 HIVE\r\n
POLL - HIVE\r\n
PING / HIVE\r\n
```

Because protocol is `HIVE`, HiveCore does not read headers or a body for these messages.

### Core-to-Worker Forwarding Framing

When HiveCore sends a request to a worker through `StreamUtil.sendRequest()`, it prefixes the serialized request with a 4-byte big-endian length:

```text
[int32 byte length]
METHOD URI PROTOCOL\r\n
header: value\r\n
...\r\n
\r\n
<optional body>
```

This applies to:

- proxied client requests
- `AUTH <nodeName> HIVE`
- `PONG / HIVE`
- `UPDATE_OLLAMA / HIVE`

Worker-side decoding is inferred from this framing code because the worker implementation is not in this repository.

## Authentication Messages

### Worker Authentication

First worker message must be:

```text
AUTH <key>;<nonce>;<hiveVersion>;<ollamaVersion> HIVE
```

Example:

```text
AUTH 2aa7f6ef-7f8d-4ef7-a9de-8db76bfa9b74;boot-20260317;1.2.0;0.6.5 HIVE
```

Server-side checks in `src/main/java/upr/famnit/managers/connections/Worker.java`:

- protocol must be `HIVE`
- method must be `AUTH`
- URI must split into exactly four `;`-separated fields
- key must verify for `VerificationType.NodeConnection`

`Overseer` then applies an additional uniqueness rule:

- if another verified worker already uses the same derived worker name but a different nonce, the new connection is rejected

On success, HiveCore sends back:

```text
AUTH <workerName> HIVE
```

Example:

```text
AUTH worker-h100-1 HIVE
```

This outbound HIVE frame is length-prefixed because it is sent through `StreamUtil.sendRequest()`.

### Client Authentication

Client auth is conditional on `USER_AUTHENTICATION`.

If enabled, inbound client requests must include:

```text
Authorization: Bearer <token>
```

Accepted roles are `Admin` or `Client`. Requests with a `Node` header require an `Admin` key because they bypass normal model-based worker selection.

### Management Authentication

Management requests always require:

```text
Authorization: Bearer <token>
```

Accepted role is `Admin` only.

## Polling, Heartbeat, and Control Messages

### Poll

Workers ask for work with `POLL`.

Default polling form:

```text
POLL mistral-nemo;bge-m3 HIVE
```

Meaning in current code:

- store `mistral-nemo;bge-m3` as the worker's tags
- look for worker-specific queued work first
- then look for model work for `mistral-nemo`
- then for `bge-m3`

Sequenced polling form:

```text
POLL - HIVE
```

Meaning in current code:

- reuse previously stored tags from earlier default polling
- prefer worker-specific queued work first
- otherwise search model queues in stored order
- rotate tag order if a later tag produced work

If no matching work exists, HiveCore sends:

```text
PONG / HIVE
```

This is also length-prefixed on the wire from core to worker.

### Ping

Workers may send:

```text
PING / HIVE
```

Current server behavior:

- update `lastPing`
- do not send an explicit reply from `handlePing()`

Any unknown HIVE method also falls through to the same logic today.

### Worker Command

Admin clients can enqueue a worker-specific control request:

```http
POST /worker/command HTTP/1.1
Authorization: Bearer <admin-token>
Content-Length: 45

{"worker":"worker-h100-1","command":"UPDATE"}
```

Current implementation maps `UPDATE` to:

```text
UPDATE_OLLAMA / HIVE
```

That request is queued to the named worker's node queue and will be delivered the next time that worker polls.

## Inbound Request Examples

### Normal Client Request

```http
POST /api/generate HTTP/1.1
Host: hivecore.example
Content-Type: application/json
Content-Length: 55

{"model":"mistral-nemo","prompt":"Why is the sky blue?"}
```

Queue classification:

- no `node` header
- `RequestQue` extracts `model` from the JSON body
- request goes to `modelQue["mistral-nemo"]`

### Node-Targeted Client Request

```http
POST /api/generate HTTP/1.1
Host: hivecore.example
Node: worker-h100-1
Content-Type: application/json
Content-Length: 55

{"model":"mistral-nemo","prompt":"Why is the sky blue?"}
```

Queue classification:

- header names are lowercased when parsed, so this becomes `node`
- request goes to `nodeQue["worker-h100-1"]`

This behavior is taken from `RequestQue.addTask()`. There is no extra admin-only gate on this path in the current code.

### Management Inspection Request

```http
GET /worker/status HTTP/1.1
Authorization: Bearer <admin-token>
Content-Length: 0

```

Expected response body shape:

```json
{
  "worker-h100-1": ["Verified", "Polling"],
  "Unauthenticated": ["SettingUp"]
}
```

## Outbound Response Examples

### Rejected Client Request

If a request is malformed or becomes unhandleable:

```http
HTTP/1.1 405 Method Not Allowed
Content-Length: 0
Connection: close

```

### Unauthorized Request

```http
HTTP/1.1 403 Unauthorized
Content-Length: 0
Connection: close

```

### Proxy Success Response

HiveCore forwards the worker's status line and headers to the client largely as-is.

Example fixed-length response:

```http
HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: 27

{"response":"hello world"}
```

Example chunked response:

```http
HTTP/1.1 200 OK
Transfer-Encoding: chunked

13
{"response":"hel
0

```

Chunk contents above are illustrative of framing only. HiveCore forwards chunk sizes and chunk payloads as read from the worker.

## Header and Body Handling Rules

Implemented header/body behavior in `StreamUtil` and `Request`:

- request header names are normalized to lowercase on input
- response headers created by `ResponseFactory` use canonical mixed-case names
- inbound request bodies require `content-length`
- worker response bodies support:
  - `transfer-encoding: chunked`
  - `content-length`
  - fallback read-until-EOF

Important caveat:

- HiveCore does not parse chunked client request bodies on ingress
- model routing depends on a simple string search for `"model"` inside the body, not on full JSON parsing

## Streaming Semantics

Streaming is supported only on the worker-response-to-client leg.

If the worker response includes `transfer-encoding: chunked`, HiveCore:

1. reads each chunk size line
2. forwards that size line to the client
3. reads exactly that many bytes
4. writes the chunk bytes to the client
5. forwards the terminating zero chunk and trailer section

If the worker response is fixed-length, HiveCore forwards bytes incrementally using a configurable buffer size from `MESSAGE_CHUNK_BUFFER_SIZE`.

If neither `transfer-encoding` nor `content-length` is present, HiveCore forwards until the worker closes the stream.

## Parsing Notes and Caveats

- `Request(Socket)` sets a 5-minute socket read timeout while parsing requests.
- `ClientRequest` also sets the client socket timeout to `PROXY_TIMEOUT_MS`; the later parse path then sets 5 minutes again inside `Request`. The resulting effective timeout depends on the call order in the current code path.
- `Overseer.sendCommand()` enqueues worker commands as synthetic node-targeted requests.
- `POST /worker/command` does not send a clear success response after successful enqueue; this is current implementation behavior.
- `Management` routes `/worker/versions`, even though one method comment still mentions `/worker/version/hive`.
- The worker protocol is only partially documented by this repository because the worker implementation is external. The length-prefixed core-to-worker framing and raw HTTP-like worker-to-core response parsing are inferred from HiveCore's side of the exchange.
