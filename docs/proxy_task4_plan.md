# Task 4 Plan — Duplicate JSON body parses for "model" (and friends)

Source issue: `docs/proxy_performance_review.md` #4 (ranked *Medium speedup / Difficulty 2 / Quality ↑*;
"also fixes drift").

> **Note — approach changed after review.** Earlier drafts planned to parse once at the top of
> `handle_connection` and **thread a `&Value` through** `authorize_request`/`plan_request` (changing
> their signatures and ~37 test call sites). Per discussion, the adopted design instead adds a
> **private memoized cache field on `HttpRequest`** so call sites and signatures stay unchanged, and we
> take the opportunity to **introduce `HttpRequest::new(...)`** to replace the ~40 ad-hoc struct
> literals (a positive refactor, not busywork). This avoids all signature churn and gives a cleaner
> constructor API.

## The problem

Every proxied inference/control request parses its JSON body into a `serde_json::Value` **multiple
times**, purely to extract routing/model fields:

1. **Admission** — `authorize_request` → `admission.rs request_models()` parses `&request.body`
   (via `extract_json_value`) for each model the key may touch (`allows_model`).
2. **Routing** — `plan_request` → planner's `json_string_field(&request.body, ...)` re-parses the body
   in `route_openai_model_request`, `route_by_required_model`, `route_systemone_eval_request`,
   `route_show_request`, `route_create_request`, `route_copy_request`, `route_push_request`,
   `route_delete_request`.
3. **Task model** — in the `QueueByNode` branch, `server.rs` calls `request_usage_model_name(&request)`
   → `extract_json_value(&request.body, ...)` — yet another parse.

So 2–3+ full `serde_json::from_slice::<Value>` passes happen per request on bodies that can be large
(chat histories, embeddings, systemone questions). Parsing is the dominant fixed cost.

## Root cause

The same body bytes are parsed by several independent helpers that each call
`serde_json::from_slice::<Value>(body)` with no sharing. There is no single "parsed once" value shared
by the admission → routing → task phases. Layering this on `HttpRequest` (whose `&self` extractors all
read the same `body`) makes a memoized cache the natural fix.

## Strategy, in two steps

### Step 1 (separate commit): introduce `HttpRequest::new` + kill ad-hoc construction

This is a **pure, safe mechanical refactor** with no behavior change, done first so the cache field
(Step 2) lands on a clean constructor foundation.

**`src/shared/http/models/http_request.rs`:**

```rust
#[derive(Clone, Debug)]        // still derived while there's no cache field yet
pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub protocol: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    // Step 2 adds a private cache field here.
}

impl HttpRequest {
    pub fn new(method: impl Into<String>, uri: impl Into<String>,
               protocol: impl Into<String>, headers: HashMap<String, String>,
               body: Vec<u8>) -> Self { ... }

    pub fn hive(method: impl Into<String>, uri: impl Into<String>) -> Self {
        // convenience for HIVE control/probe frames: empty headers + body, protocol "HIVE"
    }
}
```

- Route **all 40 literal `HttpRequest { ... }` constructions** (production + tests) through `::new`
  (or `::hive` for the synthetic HIVE/AUTH/probe frames). Every site is the uniform
  `method, uri, protocol, headers, body` shape, so a single 5-arg `new` covers them; the handful that
  pass `headers: Default::default()` / `body: Vec::new()` get a small convenience or just pass empties.
- Convert `read_request_from_reader` (`shared/http/mod.rs:331`) to build via `::new` too, so the
  parser and all callers share one path.
- Keep `#[derive(Clone, Debug)]` in Step 1 (no cache yet) — so `::new` is the *only* change and is
  trivially reviewable and safe.

### Step 2 (task #4 change): private memoized JSON cache on `HttpRequest`

**Field** (private, so external callers can't bypass it):

```rust
use std::sync::OnceLock;

pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub protocol: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    parsed_json: OnceLock<Option<serde_json::Value>>,
}
```

- **Why `OnceLock<Option<Value>>`:** lazily parsed exactly once; `None` means "parsed and it wasn't
  valid JSON" (no re-parse on every miss). `OnceLock` is `Send + Sync`, which `HttpRequest` needs
  because `ClientTask.request` is moved onto worker threads (`Rc<RefCell>` would be forbidden).
- `::new` initializes `parsed_json: OnceLock::new()`.
- Because `OnceLock<Option<Value>>` doesn't derive `Clone`/`Debug`, **hand-write `Clone` and `Debug`**
  for `HttpRequest` in Step 2 (clone/debug the four plain public fields; ignore the cache, or clone it
  only if needed for capture — see "Safety" below).

**Public read accessor** (the only surface the extractors use):

```rust
impl HttpRequest {
    /// Body parsed as JSON, memoized. Returns None for non-JSON bodies.
    pub fn parsed_json(&self) -> Option<&serde_json::Value> {
        // populate once if unset, then return
        self.parsed_json.get_or_init(|| serde_json::from_slice(&self.body).ok()).as_ref()
    }
}
```

**Callers use the cache with zero signature changes:**

- `src/shared/http/mod.rs`:
  - `extract_json_value(_body, field)` → reimplement as
    `request.parsed_json().and_then(|v| v.get(field).and_then(Value::as_str).map(str::to_string))`
    (or keep a `&Value`-taking variant and have callers pass `request.parsed_json()`).
  - Delete dead `request_model_name`.
- `src/servers/proxy/admission.rs` `request_models`: read `request.parsed_json()` instead of parsing
  `&request.body`.
- `src/servers/proxy/planner.rs` `json_string_field(body, field)` → `json_string_field(request, field)`
  that reads `request.parsed_json()` (so all 8 route helpers share the one cached parse).
- `src/servers/proxy/server.rs` `QueueByNode`: `request_usage_model_name(&request)` already gets
  `request`; have it read `request.parsed_json()`.

All these helpers keep their current call signatures; only their *internals* switch from
`serde_json::from_slice(&body)` to reading the memoized parse back.

## Route-table consolidation (as before)

- Delete dead `request_model_name`.
- Keep `request_usage_model_name` and `request_models` as **distinct route sets on purpose** (usage
  excludes admin ops like `/api/pull|push|delete`; policy check includes them). Factor the shared
  field-extraction (route → `model`/`name`/`source`/`from`) into one place and document why the lists
  differ. Do **not** blindly merge.

## Safety / correctness

- **Staleness:** the only post-parse mutation is `ensure_openai_stream_usage(&mut request)` which
  rewrites `request.body` **after** admission and routing have already read the cache. The cache holds
  the pre-mutation parse, which is exactly what admission/routing used — correct. No later code reads
  the cache expecting the new body.
- **Clone semantics:** `request.clone()` at `server.rs:130/167` (into `client_request` for capture)
  copies the four plain fields and the cache is ignored/reset in the clone. Capture uses
  `client_request` for its own serialization and never needs the parsed tree. Ensure the hand-written
  `Clone` does **not** deep-copy a populated `Value` into `client_request` (it'd duplicate body-size
  memory for nothing) — reset to an empty `OnceLock` unless a specific capture path needs it.
- **Send/Sync:** `OnceLock<Option<Value>>` is `Send + Sync`; nothing writes the cache after a task
  leaves the proxy, so shipping it to a worker is safe. Add a comment noting why a `Send` cell lives in
  an otherwise-plain struct.
- **Granularity:** caching the full `Value` (not just the "model" string) is what lets all routes
  (`model`/`source`/`from`/`name`) share a single parse. A model-string-only cache would not help
  `/api/copy` or `/api/create`.

## Files touched

- `src/shared/http/models/http_request.rs` — `::new` (+ `::hive`), private `parsed_json: OnceLock`,
  `parsed_json()` accessor, manual `Clone`+`Debug` (Step 2).
- `src/shared/http/mod.rs` — `read_request_from_reader` uses `::new`; `extract_json_value` reads cache;
  delete dead `request_model_name`.
- `src/servers/proxy/admission.rs` — `request_models` reads `parsed_json()`.
- `src/servers/proxy/planner.rs` — `json_string_field` + all route helpers read `parsed_json()`.
- `src/servers/proxy/server.rs` — `QueueByNode` model read off cache (internals only).
- ~40 `HttpRequest { ... }` literal sites → `::new`/`::hive` (Step 1), across proxy/planner/queue/
  probe/worker/management/capture/tests.

## Test impact

- **Step 1** is mechanical: compile-tests + existing suite must stay green (constructors only).
- **Step 2**: add a unit test in `http_request.rs` proving `parsed_json()` is memoized — call it twice
  and confirm the same parse / that a second call doesn't re-run (e.g. via a counter or by checking
  `OnceLock` state), plus a non-JSON body returning `None`.
- Add a planner/admission test that both see the same parsed model; a test that `/api/pull|push|delete`
  are excluded from usage extraction but included in the policy check (documents intended drift).

## Out of scope

- **Task #5** (`ensure_openai_stream_usage` re-serialize) — separate change.
- **Task #8** (request clone through queue) — separate change.
- Pre-populating the cache at parse time (`read_request_from_reader`) — optional micro-optimization;
  laziness already removes the redundancy, so not required.

## Verification

- `export PATH="$HOME/.cargo/bin:$PATH"`
- **Step 1**: `cargo test` + `cargo build --release` green (pure constructor swap).
- **Step 2**: `cargo test` (binary crate) — all existing + new tests green; `cargo build --release`
  clean; confirm no new warnings (manual `Clone`/`Debug`, dead `request_model_name` removed).
