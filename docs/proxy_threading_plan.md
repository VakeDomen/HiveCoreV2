# Plan — Replace thread-per-connection accept loops with a growable, thread-reusing pool

**Scope (proxy + management only):** `src/servers/proxy/server.rs:26-36`, `src/servers/management/server.rs:19-29`
**Explicitly out of scope for now:** the worker listener (`src/servers/worker/server.rs`) — it is stable and
untouched. **No new config.ini knobs** — use sensible hardcoded defaults.
**Status:** IMPLEMENTED (pool primitive + proxy/management listeners wired; test suite green). Plan below is
retained for reference and future tuning.
**Design requirement (from user):** threads are **unbounded** (no hard cap on active threads — connections
mostly just wait), **but** we want to **reuse** threads instead of spawning a fresh one per connection.

---

## 0. Decision: an owned, grow-on-demand thread pool — not async, not a fixed cap, not 1-thread-per-connection

The whole server stack is **blocking I/O**:
- Request parsing is `read_request_from_reader` over `BufReader<TcpStream>` (`src/shared/http/mod.rs:281-338`).
- The only HTTP client is `reqwest::blocking` (`src/servers/telegram.rs:8`).
- There is **no direct tokio / async runtime** in `Cargo.toml` (only transitive lockfile entries from optional reqwest features — not compiled).

A tokio/async rewrite would touch every module and is a much larger, riskier change than the goal requires.

**Therefore the plan is an owned **growable thread pool**, applied only to the proxy and management listeners:**
- A small number of **core/evergreen worker threads** always parked on the job queue, ready to reuse.
- When all workers are busy and the queue is non-empty, a **new worker is spawned on demand** to keep up —
  i.e. concurrency is not artificially capped (in line with "mostly it's just waiting").
- Idle workers (no job for a grace period) **retire** and exit, so the pool shrinks back toward core size under
  low load instead of leaving a massive pool of parked threads behind.
- All workers use a **small stack** (e.g. 512 KB) via `thread::Builder`, since they mostly wait.

This removes per-connection thread spawn/teardown and the 8 MB default-stack over-allocation, while keeping
unbounded concurrency under bursts.

**Worker listener is deliberately left as-is.** Its connections are long-lived, bidirectional POLL/PONG
transports; pool work would not help and risks destabilizing stable code, per feedback.

---

## 1. New shared primitive: `GrowablePool`

Add `src/shared/taskpool.rs` (and `pub mod taskpool;` in `src/shared/mod.rs`). Self-contained (~100 lines,
no external deps; only `std::sync::{Arc, Mutex, Condvar}` + `thread::Builder`).

Data:
```
struct GrowablePool {
    tx:       mpsc::Sender<Job>,             // job submission
    state:    Arc<Mutex<PoolState>>,
    stack_kb: usize,
}
struct PoolState {
    idle:        usize,                       // workers currently blocked on recv
    workers:     usize,                       // total live worker threads
    retiring:    HashMap<ThreadId, Instant>,  // workers waiting to retire
    zombie:      Vec<Job>,                    // requeue when a retiring worker sees a job
    notifier:    Condvar,                     // wake a parked worker / the spawner
}
```
- `GrowablePool::new(core: usize, stack_kb: usize)` spawns `core` workers and keeps them forever.
- `pool.spawn(Box<dyn FnOnce() + Send + 'static>)`:
  - `send` the job on `tx`, then `notify`.
  - If no idle worker is parked and we want to grow, spawn an extra worker (guarded so we never exceed the
    number of *pending* jobs with workers, i.e. `workers < idle + queued`). This is the **unbounded** growth:
    concurrency naturally tracks the number of simultaneous busy connections.
- Worker loop:
  - `recv_timeout(GRACE)` → job? run it; else it's idle → decrement `idle`, mark retiring, and wait on
    `notifier` for `GRACE` (`POOL_IDLE_TIMEOUT`). If a job arrives before timeout, run it (reuse). If the
    timeout fires with no job, decrement `workers` and exit.

### Use `Mutex<VecDeque<Job>>` + `Condvar`, not `mpsc`
A thread that is about to retire must **atomically** re-check the queue so a job racing in isn't stranded.
With a mutex-and-condvar queue this is straightforward: the retirer locks, drains one job if present, else
removes itself from `workers` and exits. This is the standard "scalable thread reuser" pattern and avoids the
mpsc time-of-check race. `spawn` simply `lock`, `push_back`, `notify_one`.

---

## 2. Proxy listener → growable pool

`src/servers/proxy/server.rs`:
- In `run(state)`, build one `GrowablePool` with **sensible default core/stack** (see §5).
- Replace `thread::spawn` in the accept loop with `pool.spawn(Box::new(move || { let _ = handle_connection(state, stream); }))`.
- Keep the existing `Arc::clone(&state)` per accept (cheap `Arc` bump). `handle_connection` already takes
  `Arc<AppState>` + `TcpStream` by move, which is exactly the job shape, so its signature is unchanged.
- Proxy handlers are short-lived and return immediately after enqueueing (the actual client socket write happens
  later on the worker thread via `ResponseTarget::ProxyClient`, `server.rs:137-158`), so a proxy thread is
  released back to the pool quickly — good for reuse and keeping burst growth modest.

---

## 3. Management listener → growable pool

`src/servers/management/server.rs`:
- Same pattern: one pool in `run(state)`, replace `thread::spawn` with `pool.spawn(|| handle_connection(state, stream))`.
- Management handlers may do SQLite reads / worker-command sends that block briefly; with a growable pool these
  simply spawn a worker if needed. Core size can be small (default 4).

---

## 4. (Removed) Worker listener — intentionally untouched

~~Convert the worker listener.~~ Not in scope. The worker server (`src/servers/worker/server.rs`) stays exactly
as it is. Rationale recorded: its threads are long-lived bidirectional transports; pooling them would occupy pool
slots permanently (never reused) and the code is stable — don't churn it. A future, separate task can revisit
stack sizing / connection caps there if ever desired. (Section removed per feedback; kept as a note only.)

---

## 5. Defaults — hardcoded, no config.ini

Per feedback, **do not add config keys**. Use `const` defaults inside `shared::taskpool.rs` / the server modules:
- `CORE_PROXY_WORKERS = 8` — evergreen recycled workers on the proxy listener.
- `CORE_MANAGEMENT_WORKERS = 4` — evergreen workers on the management listener.
- `POOL_STACK_KB = 512` — worker stack for both pools.
- `POOL_IDLE_TIMEOUT = Duration::from_secs(10)` — idle worker retires after this (controls shrink rate).

These are inline constants, not INI keys. `src/app/models/config.rs` and `src/app/config.rs` are **not touched**
for this change.

---

## 6. Implementation order & verification

1. Add `src/shared/taskpool.rs` + register in `src/shared/mod.rs`. Unit tests:
   - Submit N jobs, assert all run and peak concurrency stays ≤ `max(active, core)` (atomic counter).
   - Reuse test: submit many sequential short jobs; worker count stays stable (thread IDs reused via
     idle/retiring path), i.e. no spawn-per-job.
   - Grow test: submit M simultaneous blocking jobs; assert worker count grows beyond `core`.
   - Decay test: after jobs drain, worker count decays back toward `core` after `POOL_IDLE_TIMEOUT`.
2. Convert **management** listener first (smallest, lowest risk). Verify dashboard + `/key/me` + admin routes.
3. Convert **proxy** listener. Verify live `/api/generate` and a streaming OpenAI chat completion end-to-end.
4. Run the full test suite + `cargo build --release`. Soak: open many concurrent client connections and confirm
   the process thread count (`ps -eLf | wc -l`) stays near the reuse baseline at steady state and only grows
   transiently during bursts, then returns — not +1 thread per connection forever.

---

## 7. Risks / notes

- **`Mutex<VecDeque>` + `Condvar` is required, not `mpsc`** — the atomic re-check on retire is what makes
  "grow/decay" safe without losing jobs. Flag in code review.
- **Unbounded growth under sustained overload** could spike memory (each thread still costs a stack). Mitigation
  options if soak shows a problem: rely on the natural decay, or (if truly needed) add queue-depth-based 503
  backpressure on the proxy accept. Start infinite/unbounded per the requirement; revisit only if evidence warrants.
- **`handle_connection` signatures unchanged** — pool job is a closure, so no broad refactor.
- **Keep-alive:** proxy/management send `Connection: close` on every response (`http.rs:246`), so each request
  is one connection — ideal for pooling. If HTTP keep-alive is ever added, revisit; note in code.
- **Worker listener untouched** — any change to it is a separate future task.

## 8. Definition of done

- No `thread::spawn` remains in the **proxy** and **management** accept loops; both use a `GrowablePool`
  (unbounded growth, idle reuse, retire-on-idle).
- Worker server source **not modified**; no new config.ini keys (defaults are inline constants).
- End-to-end LLM streaming + management dashboard verified manually; full test suite green; concurrency soak
  confirms thread reuse (stable thread count at steady state) and burst growth without per-connection thread churn.
