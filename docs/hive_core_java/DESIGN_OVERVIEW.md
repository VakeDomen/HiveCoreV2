# Design Overview

## Introduction

HiveCore is a central broker for a pool of worker nodes. Clients talk to HiveCore over an HTTP-like interface, workers keep outbound connections open to HiveCore over a small HIVE control protocol, and administrators use a separate management listener to inspect and control the runtime.

The primary implementation lives in:

- `src/main/java/upr/famnit/Main.java`
- `src/main/java/upr/famnit/network/*`
- `src/main/java/upr/famnit/managers/connections/*`
- `src/main/java/upr/famnit/components/*`

## Main Components

- `Main`: loads config, initializes the SQLite key table, and starts all listeners.
- `ClientServer` and `Client`: accept client requests and enqueue them.
- `WorkerServer` and `Worker`: accept worker sockets, authenticate them, and let them poll for work.
- `Overseer`: tracks connected workers, applies timeouts, finalizes worker verification, and rejects queued work that no current worker can handle.
- `ManagementServer` and `Management`: expose admin-only inspection and command endpoints.
- `RequestQue`: stores queued work by model name and by worker name.
- `Connection`: owns per-worker socket I/O and performs request/response proxying.
- `DatabaseManager` and `KeyUtil`: back bearer-key authentication with SQLite plus an in-memory cache.

## High-Level Lifecycle

On startup, HiveCore binds three ports and starts accepting connections. Client requests are parsed immediately and stored in memory. Worker connections are long-lived: each worker authenticates once, then repeatedly sends HIVE `POLL` or `PING` messages. When a worker polls, HiveCore dequeues a compatible request and forwards it to that worker. The worker's HTTP response is then streamed back to the original client socket.

Management requests are separate one-shot HTTP-like requests. They inspect the worker registry, queue depths, stored keys, or enqueue a worker-specific command.

## Main Data and Control Flows

Client flow:

1. A client connects to the proxy port.
2. HiveCore parses the request and optionally checks a bearer token if `USER_AUTHENTICATION` is enabled.
3. The request is queued either by `node` header or by `model` extracted from the JSON body.

Worker flow:

1. A worker connects to the worker port.
2. It authenticates with `AUTH key;nonce;hiveVersion;ollamaVersion HIVE`.
3. `Overseer` verifies that the worker name is not already in use with a different nonce.
4. The worker enters a poll loop.
5. On `POLL`, HiveCore selects node-specific work first, then model work.

Proxy flow:

1. HiveCore writes the selected client request to the worker socket with a 4-byte length prefix.
2. HiveCore reads an HTTP-like response back from the worker.
3. HiveCore forwards status line, headers, and body directly to the client.

Management flow:

1. An admin sends an HTTP-like request with `Authorization: Bearer <token>`.
2. HiveCore checks that the token resolves to `Role.Admin`.
3. The handler returns JSON state or enqueues a worker command.

## Operational Modes

The runtime effectively has three modes running at once:

- admission mode on the proxy listener
- worker coordination mode on the worker listener plus `Overseer`
- administrative inspection/control mode on the management listener

Within worker coordination, there are two polling modes:

- default polling: worker supplies its currently available tags in the `POLL` URI
- sequenced polling: worker sends `POLL - HIVE`, and HiveCore reuses previously stored tags while rotating preferred tag order based on recent matches

## Failure Model

HiveCore is in-memory and connection-oriented. Important consequences:

- queued requests are lost on process restart
- a failed worker request is not retried onto another worker
- proxy failures become `502` only if HiveCore has not already started writing the response to the client
- `Overseer` actively removes timed-out or rejected workers
- `Overseer` also drains queued requests that no currently connected worker can satisfy and returns `405`

Authentication and authorization are also minimal:

- worker auth requires a key with `Admin` or `Worker` role
- management auth requires `Admin`
- client auth is optional and gated only by `USER_AUTHENTICATION`

One code-backed caveat: targeted proxy requests with a `node` header are not separately restricted to admin callers in the current implementation, even though the README describes that behavior.

## Key Design Constraints

- Worker nodes are pull-based. HiveCore never initiates arbitrary work without a worker poll.
- The worker data plane is asymmetric: core-to-worker requests are length-prefixed, while worker-to-core responses are parsed as plain HTTP-like streams.
- Queue routing is simple and local. There is no scheduler beyond queue choice and poll order.
- Shared state is process-local. There is no clustering, replication, or durable queue backend.
- Parsing is intentionally lightweight. For example, model extraction uses string scanning rather than full JSON decoding.
- Admin APIs reflect current in-memory state, not a historical record.
