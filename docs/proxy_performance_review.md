# Proxy Performance Review — "Stupid" Things Where We're Giving Up Time

Scope: the proxy request path (`src/servers/proxy/*`, `src/servers/worker/server.rs` response relay,
plus the hot shared code they call: `src/shared/http/mod.rs`, `src/auth/*`, `src/app/rate_limiter.rs`).
No fixes applied — this is the identification/prioritization pass.

Ranking legend:
- **Speedup**: how much wall-clock we realistically gain on the request path (LLM work dwarfs most
  of this, but connection setup, parsing, locking, cloning and logging are pure overhead on every request).
- **Difficulty of implementation** (1 = trivial, 5 = substantial).
- **Code quality impact**: mostly negative today, mostly positive after.

---

## 1. Thread-per-connection accept loop (proxy + worker + management)
**Location:** `src/servers/proxy/server.rs:26-36`, `src/servers/worker/server.rs:30-40`
**Speedup:** High at high concurrency; latency neutral at low load. **Difficulty:** 3 **Quality:** ↑

`listener.incoming()` spawns a fresh OS thread per accepted TCP connection on every listener. Two problems:
- Each LLM client request opens a connection, so we pay thread spawn/teardown + 2 stack allocations per request.
- High fan-out (many browsers/clients, each holding a keep-alive or an idle poll) burns RAM (default 8 MB stacks
  per thread) and forces the scheduler to thrash.

The worker side is the worse offender: every worker keeps a long-lived connection open and calls `POLL`
repeatedly — that's fine — but the *proxy* side spawning one thread per client request is pure overhead.
Migrating the listeners to an async runtime (tokio) or at least a bounded thread pool would cap these.

## 2. `KeyStore::verify` clones the whole `KeyRecord` per request on admission
**Location:** `src/auth/keys.rs:24-47` (specifically `guard.get(token).cloned()` and re-clone on cache fill)
**Speedup:** Medium (small absolute, but every single request). **Difficulty:** 1 **Quality:** ↑

Every proxied request does `authorize_request -> authorized_key -> keys.verify`, which on a cache hit does
`guard.get(token).cloned()` — a full deep clone of `KeyRecord` (token `String`, name `String`, two `Vec<String>`
whitelist/blacklist, plus role). Then in `server.rs` we clone again via `visible_key.as_ref().map(...)` and store
`key_name.clone()` into the task context. So per request we allocate/deep-copy the key's strings potentially 3+ times.

Better: verify returns `Arc<KeyRecord>` (share, don't clone) or a borrow with an explicit lifetime; the cache is an
append/refresh-only `RwLock<HashMap>` so an `Arc` is safe and near-free.

## 3. Header lookup allocates a lowercase `String` per access
**Location:** `src/shared/http/models/http_request.rs:13-30` (`header()`, `bearer_token()`)
**Speedup:** Low–medium per call, but this runs many times per request. **Difficulty:** 1 **Quality:** ↑

`HttpRequest.header(name)` does `self.headers.get(&name.to_ascii_lowercase())` — allocating a new `String` on
*every* lookup. `bearer_token()` calls `header` at least twice (authorization, then api-key); `authorize_request`
calls `request.header("node")`; `plan_request` calls `request.header("node")` again; `ensure_openai_stream_usage`
idempotent walks. Easily 4-6 lowercase allocations per request before any work happens.

Headers are already normalized to lowercase when parsed (`read_request_from_reader` inserts `to_ascii_lowercase`),
so the lookup keys could be `&str` comparisons against pre-lowercased constants, or headers kept as a
case-insensitive map. At minimum, cache the bearer token once.

## 4. Re-parsing the JSON body for "model" over and over
**Location:** `src/shared/http/mod.rs` (`request_model_name`, `request_usage_model_name`), `src/servers/proxy/admission.rs:128` (`request_models`), `src/servers/proxy/planner.rs:670` (`json_string_field`)
**Speedup:** Medium. **Difficulty:** 2 **Quality:** ↑

The same JSON body is parsed (`serde_json::from_slice::<Value>`) multiple times per request to extract the model
name: once in `authorize_request -> request_models`, once in the planner via `json_string_field`, and once more in
`server.rs` via `request_usage_model_name` for the task model. That's 2-3 full `serde_json::Value` parses + the
`String` allocations for the model name each time — on every inference request, for bodies that can be large
(chat histories, embeddings, systemone questions).

Better: parse the body **once** in admission, extract each needed field, and hand the parsed value / model name
down through planner and into the `ClientTask` (it's already stored). This also removes the duplicated
route-match tables (`request_models`, `request_usage_model_name`, `request_model_name`, planner's
`json_string_field`) which have already drifted (e.g. `request_models` includes `/api/pull|push|delete` while
`request_usage_model_name` excludes them) — a correctness/latency cleanup in one.

## 5. `ensure_openai_stream_usage` — unconditional full re-serialize on every OpenAI call
**Location:** `src/shared/http/mod.rs:83-130`
**Speedup:** Medium for OpenAI traffic. **Difficulty:** 2 **Quality:** ↑

For every `/v1/chat/completions` and `/v1/completions` request (the bulk of OpenAI-compatible traffic) this builds
a full `serde_json::Value` tree, then re-serializes the *entire body* back to bytes even when the request is
non-streaming (it returns `false` only *after* parsing and, for non-stream, after constructing the value but
before re-serializing — good), but for streaming it mutates then calls `serde_json::to_vec` on the whole body.
`ensure_openai_stream_usage` is invoked unconditionally in both queue branches. This couples a whole-body parse +
reserialize + replace to the request hot path — on top of issue #4's parses.

Better: do this only when `stream: true` is detected cheaply, and prefer a small targeted textual edit of the JSON
to inject `stream_options.include_usage` without a full parse/reserialize. At minimum gate the full parse behind a
cheap `contains("stream")` check.

## 6. `dequeue_for_worker` lock ordering + O(tags × kinds) scan every poll
**Location:** `src/servers/proxy/queue.rs:86-164`
**Speedup:** Medium under load (workers poll constantly). **Difficulty:** 3 **Quality:** ↑

Every worker poll does: lock `node_queue`, then lock `model_queue` (two separate mutexes, always taken in the same
order — fine — but they're two locks serializing all dispatch). For each of the worker's tags it iterates the 4
`ModelRouteKind`s and does a hash lookup per (tag, kind) pair, building a `ModelQueueKey` (a fresh `String` + kind)
per candidate in `next_compatible_key:289-313`. With many workers all polling, this is the global dispatch
bottleneck.

Better: index the model queue by a simpler key (model + kind hashed once), avoid re-allocating the key per probe,
and ideally condense to a single mutex or a sharded structure. The lock is coarse but functionally OK; the
allocations and the re-probe pattern are the win.

## 7. `write!`/`format!` per header/byte when serializing and relaying responses
**Location:** `src/servers/worker/server.rs` (`proxy_worker_response_with_capture:527-531`, `sanitize_response_headers`,
`relay_chunked_body`), and `src/shared/http/mod.rs:270-278` (`HttpResponse::write_to`)
**Speedup:** Medium for streaming responses (each chunk). **Difficulty:** 2 **Quality:** ↑

Response relay writes header fields one `write!` at a time (many tiny `write!` calls per header, then per chunk it does
a `read_line` into a `String`, `write_all` the line, `read_exact` into a fresh `vec![0; size]`, `write_all`, etc.).
Each streaming chunk allocates a new `Vec` for the body and `String` for the size line. For streaming LLM output the
per-chunk overhead is real.

Better: reuse a single scratch buffer for chunks (or a fixed stack/thread-local buffer), and write headers into a
single pre-formatted buffer with one `write_all`.

## 8. Frame-level double buffering/cloning through the queue
**Location:** `src/servers/proxy/server.rs:123-159` and `queue.rs enqueue_model/enqueue_node:28-84`, plus
`src/servers/proxy/models/client_task.rs`
**Speedup:** Low–medium. **Difficulty:** 2 **Quality:** ↑

Look at what's copied on a queued request:
- `server.rs` clones `request` wholesale into `client_request` (`let client_request = request.clone();`) *and* keeps
  the original `request` for the task — two full copies of the `HttpRequest` (including body `Vec<u8>`).
- `enqueue_model`/`enqueue_node` then clone `task.request.method`, `.uri`, and `context.key_name` just to build log
  strings *before* pushing — all allocations on the enqueue path.
- `worker/server.rs` then calls `serialize_request(&task.request)`, which re-formats the request into bytes.

The body is copied at least twice from the socket read to the worker socket. For big payloads (embedding,
systemone questions) this is wasted bytes moved. Minor compared to the backend work, but it's the definition of
"stupid" overhead — the clone for logging (`method`/`uri`/`user`) inside the locked push is pure waste.

## 9. Probe aggregation spawns a real OS thread per worker, per request
**Location:** `src/servers/proxy/planner.rs:688-733` (`parallel_probe_json`), `probe.rs`
**Speedup:** Medium only for these admin/aggregate routes (`/api/tags`, `/v1/models`, `/api/version`).
**Difficulty:** 3 **Quality:** ↑

`/api/tags`, `/v1/models`, `/api/ps`, `/api/version`, `/health` fan out one `thread::scope` OS thread **per worker**
and block on an mpsc channel — for a lightweight metadata fetch. With N workers that's N thread spawns per request,
plus serializing each probe through the queue (`dispatch_capture` enqueues to the node and `recv_timeout`s). These
are lower-traffic routes, so the impact is bounded, but the implementation is heavy. Note the code *does* have an
aggregate cache (`probe.rs cached_value`) but it's only used when the worker has no polling connection — otherwise
it round-trips to the worker on every `/api/tags`.

## 10. Per-request logging does multiple `SystemTime::now()` + `format!` + surrounding allocations
**Location:** `src/shared/log/mod.rs`, called from `server.rs`, `queue.rs`, `worker/server.rs`
**Speedup:** Low–medium (can be surprisingly high if stdout is a bottleneck). **Difficulty:** 1 **Quality:** ↑

Every request logs: "queued" (`queue.rs`), "forwarding" + "resolved" + possibly "usage missing"
(`worker/server.rs`), and the proxy "served/rejected" line (`server.rs`). Each `log::*` calls `SystemTime::now()`
and `format_local_time` (a `localtime_r` syscall), plus the many `format!`/`bold()` allocations to compose the
message, then a synchronous `println!`. On the proxy thread and the worker relay thread this is on the hot path and
serializes on `stdout`'s lock.

Better: async/level-gated logging with a single timestamp per request, and avoid `bold()`/`format_duration()`
allocating when the message is discarded. At minimum, a structured logger with a level filter.

## 11. `RwLock<HashMap>` for the worker registry is read+written on every poll and every dispatch
**Location:** `src/app/state.rs:17`, `src/servers/worker/server.rs` (`touch_worker`, `dequeue_for_worker` callers),
`src/servers/proxy/planner.rs` (`connected_workers*`, `model_owners`)
**Speedup:** Low–medium. **Difficulty:** 2 **Quality:** ↑

`touch_worker` takes an exclusive `write()` lock on the whole workers map on *every* poll, ping, and dispatch to
update phase/timestamps, and `dequeue_for_worker`'s caller re-reads it. On top, `planner`'s `model_owners` /
`connected_workers` scan all worker `tags` linearly (a `Vec<bool>`-style membership test `tags.iter().any(...)`)
to decide routing — for every request. With many workers this is a shared lock churn plus O(workers×tags) routing.

Better: keep per-worker (or per-connection) mutable state independent of the shared registry (an `Arc<RwLock>`
per worker), and maintain a model→workers index to make `model_owners` O(1)-ish instead of linear scans.

## 12. Rate limiter: one global mutex, `Instant::now()` per call, and a `Vec<Instant>` per window that grows
**Location:** `src/app/rate_limiter.rs:116-211`
**Speedup:** Low on the happy path; the lock is global and serializes all admission. **Difficulty:** 4 **Quality:** ↑

`check()` and `start_request()`/`finish_request()` each lock one global `Mutex<HashMap<i64, KeyState>>`, so every
request's admission and completion serialize on that one lock — including the entire sliding-window bookkeeping
(`prune_and_count` binary search + `push` per window × 5 windows, across 5 tiers). Combined with #2/#3 this makes the
"auth" stage the most lock-heavy part of the proxy. The `Vec<Instant>` per window also grows unboundedly with volume
in a minute (each request pushes an `Instant`), so a hot key holds a big vec.

Better: shard by key, use the monotonic time once per batch, and use a token-bucket or fixed-window counter array
instead of a timestamp `Vec` per window.

---

## Summary ranking

| # | Area | Speedup | Difficulty | Quality |
|---|------|---------|-----------|---------|
| 1 | Thread-per-connection accept loops | High (concurrency) | 3 | Great win |
| 2 | Key clone per request (`KeyRecord`) | Medium | 1 | Easy, clean |
| 3 | Header lookup lowercase-`String` alloc | Low–med | 1 | Easy, clean |
| 4 | Duplicate JSON body parse for "model" | Medium | 2 | Also fixes drift |
| 5 | `ensure_openai_stream_usage` full re-serialize | Medium (OpenAI) | 2 | Easy, clean |
| 6 | Queue dispatch: 2 locks + O(tags×kinds) + per-probe key alloc | Medium | 3 | Moderate |
| 7 | Response relay: per-chunk `Vec`/`String` + many small `write!` | Medium (streaming) | 2 | Moderate |
| 8 | Request body copied/cloned 2-3× through queue+logs | Low–med | 2 | Easy, clean |
| 9 | Probe fan-out: OS thread per worker | Medium (admin routes) | 3 | Moderate |
| 10 | Per-request logging on hot path | Low–med | 1 | Easy, clean |
| 11 | Global worker `RwLock` + linear tag scans | Low–med | 2 | Moderate |
| 12 | Global rate-limit mutex + per-window `Vec<Instant>` | Low | 4 | Larger refactor |

**Best effort-per-hour ratio:** #2, #3, #5, #4, #10, #8 are all small, contained, and immediately shave per-request
overhead. #1, #6, #11 touch the concurrency model and give the biggest wins under real load (many workers, many
clients) but deserve their own pass. #12 is the one that's most coupled to a redesign.

The single most "stupid" thing relative to its cost is probably **#2 + #3 + #4 together**: on every request we
clone the key, allocate lowercase header strings, and parse the JSON body 2-3 times — all before doing anything
useful — and all are one-line-ish fixes.
