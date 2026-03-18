# Technical Specification

## Purpose

HiveCore is a Java 21 process that fronts a pool of outbound-connected worker nodes and exposes:

- a client-facing HTTP-like proxy on the configured proxy port
- a worker-facing control/data channel on the configured worker port
- an admin-facing HTTP-like management API on the configured management port

Its primary runtime responsibility is to accept client requests, classify them into queues, let authenticated workers poll for work, forward selected requests to a worker, and stream the worker's HTTP response back to the original client.

This behavior is implemented in `src/main/java/upr/famnit/Main.java`, `src/main/java/upr/famnit/network/*.java`, `src/main/java/upr/famnit/managers/connections/*.java`, and `src/main/java/upr/famnit/components/*.java`.

## Scope

This specification covers the current implementation of the long-running proxy/service process in this repository. It focuses on:

- process startup and listener threads
- worker connection authentication and lifecycle
- client request admission and queueing
- worker polling, request dispatch, and response proxying
- management endpoints
- shared state, locking, and failure behavior

It does not specify worker-side implementation details that are not present in this repository. Where worker behavior is inferred, that is called out explicitly.

## High-Level Architecture

`src/main/java/upr/famnit/Main.java` performs three startup actions:

1. Loads configuration from `config.ini` via `src/main/java/upr/famnit/util/Config.java`.
2. Ensures the SQLite `keys` table exists via `src/main/java/upr/famnit/managers/DatabaseManager.java`.
3. Starts three listener threads:
   - `WorkerServer` on `NODE_CONNECTION_PORT`
   - `ClientServer` on `PROXY_PORT`
   - `ManagementServer` on `MANAGEMENT_CONNECTION_PORT`

The primary runtime component is the combined proxy service made up of:

- `ClientServer`: accepts inbound client requests and converts them to queued `ClientRequest` instances.
- `WorkerServer`: accepts worker sockets and runs one `Worker` thread per connection.
- `Overseer`: a background thread started by `WorkerServer` that tracks worker health and rejects queue items that no connected worker can currently satisfy.
- `ManagementServer`: exposes admin-only inspection and control endpoints backed by the same in-memory state.

## Runtime/Process Model

HiveCore is a single JVM process with multiple long-lived threads and cached thread pools.

- `Main` starts the three top-level server threads and then blocks on `join()`.
- `ClientServer` runs an accept loop and submits `Client` runnables to a cached executor.
- `WorkerServer` runs an accept loop, submits each `Worker` thread to a cached executor, and registers that worker with `Overseer`.
- `ManagementServer` runs an accept loop and submits `Management` runnables to a cached executor.
- `Overseer` is a separate long-lived thread that wakes every 500 ms.

There is no explicit coordinated shutdown path from `Main`. The server classes each have `shutdown()` methods, but `Main` never calls them.

## Concurrency Model

The runtime uses coarse-grained thread-per-connection handling plus concurrent queues/maps for shared routing state.

- Client connections are handled concurrently by `Executors.newCachedThreadPool()` in `ClientServer`.
- Worker connections are handled concurrently by `Executors.newCachedThreadPool()` in `WorkerServer`.
- Management connections are handled concurrently by `Executors.newCachedThreadPool()` in `ManagementServer`.
- Worker registry access is protected by a process-wide `ReentrantReadWriteLock` in `Overseer`.
- Per-worker mutable metadata is protected by a `ReentrantReadWriteLock` inside `NodeData`.
- Per-worker socket I/O is serialized by a `ReentrantLock` in `Connection`.
- Work queues are `ConcurrentHashMap<String, ConcurrentLinkedQueue<ClientRequest>>` in `RequestQue`.
- Key lookup cache is a `ConcurrentHashMap<String, Key>` in `KeyUtil`.
- Database operations are synchronized static methods in `DatabaseManager`.

The effective model is:

- many client admission threads enqueue work
- many worker threads poll and dequeue work
- one overseer thread supervises workers and periodically drains requests it deems unhandleable

## Configuration Contract

Configuration is loaded from `config.ini` by `Config.init()` in `src/main/java/upr/famnit/util/Config.java`.

Recognized settings are:

- `[Server] USER_AUTHENTICATION`
- `[Server] PROXY_PORT`
- `[Server] NODE_CONNECTION_PORT`
- `[Server] MANAGEMENT_CONNECTION_PORT`
- `[Connection] CONNECTION_EXCEPTION_THRESHOLD`
- `[Connection] POLLING_NODE_CONNECTION_TIMEOUT`
- `[Connection] WORKING_NODE_CONNECTION_TIMEOUT`
- `[Connection] PROXY_TIMEOUT_MS`
- `[Connection] MESSAGE_CHUNK_BUFFER_SIZE`
- `[Database] DATABASE_URL`

Notable implementation details:

- `SETTING_UP_NODE_CONNECTION_TIMEOUT` exists in code but is not loaded from `config.ini`; it stays at the class default unless the code changes.
- `CONNECTION_EXCEPTION_THRESHOLD` is loaded but not used anywhere in the current runtime path.
- If `config.ini` is missing, `Config` writes one with defaults.

The repository's checked-in `config.ini` currently sets:

- proxy port `6666`
- worker port `7777`
- management port `6668`
- client auth disabled
- proxy socket timeout `60000` ms
- worker poll timeout `10` s
- worker working timeout `300` s
- SQLite URL `jdbc:sqlite:sqlite.db`

## Runtime/Service Lifecycle

### Startup

At startup:

1. Configuration is loaded or created.
2. The SQLite `keys` table is created if absent.
3. Sockets are bound for the three listeners.
4. The worker monitor thread starts.
5. Accept loops run indefinitely.

If any startup exception escapes `Main`, it is logged and printed, and the process does not recover.

### Steady State

During normal operation:

- clients submit HTTP-like requests to the proxy listener
- workers keep long-lived sockets open and send HIVE control messages
- workers poll for work
- selected client requests are forwarded to workers
- worker HTTP responses are streamed back to the original client socket
- management clients inspect queue and worker state or enqueue worker-specific commands

### Shutdown

There is no integrated shutdown coordinator. Each server class exposes a local `shutdown()` method, but these are not wired into signal handling or `Main`.

## Connection Lifecycle

### Client Connection Lifecycle

Client connections arrive at `ClientServer`, which constructs a `Client` runnable by accepting the socket inside the `Client` constructor.

The `Client` handler:

1. Wraps the socket in a `ClientRequest`.
2. Parses the HTTP-like request from the socket using `Request`.
3. Optionally authorizes it if `USER_AUTHENTICATION` is `true`.
4. Adds it to either a model queue or a node queue via `RequestQue`.
5. Leaves the socket open for the eventual worker response.

If request admission fails:

- authentication failure returns `403 Unauthorized`
- invalid request structure returns `405 Method Not Allowed`
- low-level read failure logs and returns without sending a response

### Worker Connection Lifecycle

Worker sockets arrive at `WorkerServer`, which accepts them inside the `Worker` constructor and wraps them in a `Connection`.

A `Worker` thread then:

1. Waits for a first message.
2. Requires that first message to be `AUTH <uri> HIVE`.
3. Parses the auth URI as `key;nonce;hiveVersion;ollamaVersion`.
4. Verifies the key role via `KeyUtil.verifyKey(..., VerificationType.NodeConnection)`.
5. Sets `NodeData` fields and enters `VerificationStatus.Waiting`.
6. Busy-waits up to 10 seconds for `Overseer` to either verify or reject the worker.
7. On verification, sends `AUTH <nodeName> HIVE` back to the worker.
8. Enters the main HIVE message loop.

The main worker loop accepts only HIVE messages. Implemented methods are:

- `POLL`
- `PING`

Any other HIVE method currently falls through to `handlePing()`.

### Management Connection Lifecycle

Management connections are one-request-per-connection handlers:

1. `Management` parses a `ClientRequest`.
2. It checks for `Authorization: Bearer <token>`.
3. It requires that token to resolve to `Role.Admin`.
4. It dispatches by URI and method.
5. It sends an HTTP response and exits.

## Message Framing and Parsing

### External Client and Management Parsing

Incoming client and management requests are parsed by `Request(Socket)` as HTTP-like text:

- first line must be `METHOD URI PROTOCOL`
- headers are read until a blank line
- header names are lowercased during parsing
- body is read only when `content-length` is present
- chunked request bodies are not supported on inbound parsing

If the parsed protocol token is `HIVE`, the parser stops after the request line and does not read headers or a body.

### Worker-to-Core Control Messages

Workers send unframed line-oriented HIVE messages that fit `Request(Socket)`:

- `AUTH key;nonce;hiveVersion;ollamaVersion HIVE`
- `POLL <uri> HIVE`
- `PING / HIVE`

Because `Request(Socket)` treats `HIVE` as headerless/bodyless, the full message is contained in the request line only.

### Core-to-Worker Forwarded Requests

When a worker receives real client work, `Connection.sendRequest()` prepends a 4-byte Java `DataOutputStream.writeInt(...)` length before the serialized request bytes.

The forwarded frame format is therefore:

1. 4-byte big-endian length
2. ASCII/UTF-8 request line
3. zero or more header lines
4. blank line
5. optional body

This framing is only visible on the server-to-worker forwarded request path and on server-originated HIVE requests sent with `sendRequest()`, including:

- `PONG / HIVE`
- `AUTH <nodeName> HIVE`
- `UPDATE_OLLAMA / HIVE`

Worker-side decoding logic is not present in this repository, so the exact worker parser is inferred from `StreamUtil.sendRequest()`.

### Worker-to-Core Response Parsing

After forwarding a client request to a worker, HiveCore reads a worker response as plain HTTP-like text without a length prefix:

- status line first
- headers until a blank line
- body by `transfer-encoding: chunked`, else `content-length`, else until EOF

This means the worker data plane is asymmetric:

- core to worker requests are length-prefixed
- worker to core responses are parsed as raw HTTP-like streams

## Control Flow and Command Handling

### Client Request Classification

`RequestQue.addTask()` routes admitted client requests as follows:

- if protocol is `HIVE`, reject immediately
- if header `node` exists, enqueue into `nodeQue`
- otherwise, extract `model` from the JSON body and enqueue into `modelQue`

Model extraction uses `StreamUtil.getValueFromJSONBody()`, which is a string search helper rather than a full JSON parser. It works for straightforward JSON bodies but should be treated as a parser shortcut.

### Worker Poll Handling

`Worker.handlePollRequest()` updates worker state and chooses queue strategy based on the HIVE poll URI:

- `POLL - HIVE` uses sequenced polling
- any other `POLL <tag-list> HIVE` uses default polling

Default polling:

- stores the URI as the worker tag string
- splits it on `;`
- checks node-specific work first, then per-model queues in listed order

Sequenced polling:

- reuses the previously stored worker tags from `NodeData`
- checks node-specific work first
- otherwise searches model queues in current tag order
- if a later tag produced work, rotates the stored tag list so that model becomes first next time

This is an in-memory locality heuristic intended to reduce model swaps on the worker.

### Management Commands

Implemented management routes in `Management.run()` are:

- `GET /key`
- `POST /key`
- `GET /worker/connections`
- `GET /worker/status`
- `GET /worker/pings`
- `GET /worker/tags`
- `GET /worker/versions`
- `POST /worker/command`
- `GET /queue`

`POST /worker/command` accepts JSON matching `WorkerCommand`:

```json
{
  "worker": "worker-name",
  "command": "UPDATE"
}
```

Currently only `UPDATE` is implemented. It creates a synthetic HIVE request `UPDATE_OLLAMA / HIVE`, wraps it in a `ClientRequest`, and enqueues it to the target worker's node queue through `Overseer.sendCommand()`.

An important operational detail: successful worker command submission returns `null` from `Overseer.sendCommand()` and `Management.handleWorkersCommandRequest()` does not send an immediate HTTP success response in that case. The connection is left without an explicit response from this handler. That is current implementation behavior, not an omitted detail in this specification.

## Request Forwarding/Proxy Flow

The primary data path is:

1. Client sends an HTTP-like request to `ClientServer`.
2. `Client` parses and optionally authorizes it.
3. `RequestQue` enqueues it by node or model.
4. A worker sends `POLL`.
5. `Worker` dequeues a matching `ClientRequest`.
6. `Connection.proxyRequestToNode()` forwards the original request to the worker with a 4-byte length prefix.
7. HiveCore reads the worker's HTTP-like response.
8. HiveCore streams the status line, headers, and body directly to the original client socket.
9. Timing stamps are updated for queue time, proxy time, and total time logging.

If proxying fails before response headers are written to the client, HiveCore sends `502 Bad Gateway`. If a runtime exception occurs later in the method, it attempts `500 Internal Server Error`.

## State Management

### Worker Registry

`Overseer` owns a static process-wide `ArrayList<Worker>` called `nodes`.

Each `Worker` holds:

- a `Connection`
- a `NodeData`

The registry is used for:

- authentication finalization
- timeout-based connection culling
- queue rejection decisions
- management API inspection
- worker command routing

### Queue State

`RequestQue` owns two static concurrent maps:

- `modelQue`: model name to queue
- `nodeQue`: worker name to queue

Queue items are `ClientRequest` instances that retain the original client socket until a worker responds or an error response is emitted.

### Auth and Key State

Persistent auth state lives in SQLite table `keys` with columns:

- `id`
- `name`
- `value`
- `role`

Hot key lookups are cached indefinitely in `KeyUtil.cache`.

## Locking/Synchronization

Relevant synchronization points:

- `Overseer.nodes` is protected by a read/write lock.
- `NodeData` fields are protected by a read/write lock.
- `Connection` serializes socket reads and writes with `socketLock`.
- `DatabaseManager` public operations are `synchronized`.
- Queue maps rely on `ConcurrentHashMap` and `ConcurrentLinkedQueue`.

One implementation caveat:

- `Overseer.checkOnQueue()` iterates `nodes` without taking the overseer read lock. Because `nodes` is a plain `ArrayList`, this is a concurrency risk in the current code.

## Error Handling and Recovery

### Startup Errors

- config, database, and thread startup failures escape to `Main`, are logged, and terminate startup

### Client Admission Errors

- bad auth when client auth is enabled: `403`
- malformed or unclassifiable request: `405`
- read failure before classification: logged, no guaranteed response

### Worker Auth Errors

- invalid first message
- invalid auth field count
- invalid key
- duplicate verified node name with different nonce
- timeout waiting for overseer verification

These cause the worker to be marked rejected or closed and the socket to be closed.

### Worker Runtime Errors

- request parse failure from worker logs as protocol violation and closes the connection
- request handling exceptions are logged; the worker loop continues unless the connection is later closed
- overseer closes workers on timeouts or rejected verification state

### Proxy Errors

- non-200 worker responses are logged but still forwarded to the client
- mid-proxy I/O failures produce `502` only if headers have not yet been sent
- missing `content-length` and missing `transfer-encoding` cause body forwarding until EOF

### Queue Rejection

Every 500 ms, `Overseer.checkOnQueue()` computes:

- active worker names
- union of advertised worker tags

It then removes queued requests that target:

- a node name with no connected worker of that name
- a model name not advertised by any connected worker

Rejected requests receive `405 Method Not Allowed`.

This is an implementation choice, not a passive queue. Requests can be dropped quickly when no currently connected worker can service them.

## Observability/Logging/Metrics Hooks

There is logging throughout the runtime via `src/main/java/upr/famnit/util/Logger.java`.

Observed log categories include:

- network/server startup and connection events
- authentication success/failure
- queue insertions
- proxying failures
- successful completion timing
- database actions

There is no metrics subsystem, tracing, or persistent event store in the current code. Timing data exists only as per-request log lines generated after successful or failed proxy attempts.

## Sequence Summaries

### Worker Authentication

1. Worker connects to `WorkerServer`.
2. Worker sends `AUTH key;nonce;hiveVersion;ollamaVersion HIVE`.
3. `Worker` validates key role and records metadata.
4. `Overseer` observes the worker in `Waiting`.
5. `Overseer` rejects duplicate verified node names with mismatched nonce, otherwise marks worker `Verified`.
6. `Worker` sends `AUTH <nodeName> HIVE` back to the worker.

### Client Request to Worker Response

1. Client sends HTTP-like request to proxy port.
2. `ClientRequest` parses request and maybe enforces bearer auth.
3. Request is queued by `node` header or by JSON `model`.
4. Worker sends `POLL`.
5. Matching request is dequeued.
6. Request is forwarded with length-prefixed framing to worker.
7. Worker returns HTTP-like response.
8. HiveCore forwards status, headers, and body to client.

### Management Inspection

1. Admin sends HTTP-like request with bearer token.
2. `Management` validates `Role.Admin`.
3. Handler reads in-memory state or SQLite keys.
4. Response is returned as JSON or empty-body status.

## Non-Goals and Explicit Boundaries

Current code does not implement:

- load balancing beyond queue order and worker polling order
- durable request storage
- retry or replay after worker failure
- inbound chunked request parsing from clients
- TLS, HTTPS termination, or certificate-based auth
- request cancellation propagation
- graceful coordinated shutdown
- cache eviction for key lookups

Also note a code/README mismatch:

- The README says targeted client requests require admin authorization, but `RequestQue.addTask()` only checks for the presence of the `node` header. No separate admin-only gate exists on the proxy path as implemented today.
