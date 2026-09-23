# Task 2 Plan — `KeyStore::verify` clones the whole `KeyRecord` per request

Source issue: `docs/proxy_performance_review.md` #2 (ranked *Medium speedup / Difficulty 1 / Easy, clean*).

## The problem

`verify` deep-clones the entire `KeyRecord` on the admission hot path:

- `src/auth/keys.rs:24-47` — cache hit does `guard.get(token).cloned()` (full deep clone: `token String`,
  `name String`, two `Vec<String>` whitelist/blacklist, `role`, etc.). Cache miss does
  `guard.insert(record.token.clone(), record.clone())` — clones again.
- Callers then clone still more:
  - `authorized_key` (`proxy/admission.rs:122`) returns `Option<KeyRecord>` and the hot path
    (`proxy/server.rs:147-150, 185-188`) re-clones `key.name.clone()` into the `ClientTask` context, and
    reads `key.id` / `key.capture` off the owned copy.
  - `authorize_management` (`app/state.rs:46-55`) reads `record.role` off the clone.
  - worker `server.rs:128-136` clones `record.name.clone()`.

So a single request can deep-copy the key's strings 3+ times before any useful work.

## Concurrency-safety analysis (why `Arc` is sound here)

The cache is `RwLock<HashMap<String, KeyRecord>>` and is **append/refresh-only**:

- `refresh_cache` replaces the whole map under `write()` from the DB (`new`, `update_key`, `delete`).
- `insert` adds a record under `write()`.
- No path mutates an existing `KeyRecord` **in place** inside the map after it is inserted — every mutation
  rebuilds via a fresh DB fetch + map replacement.

Therefore existing entries are effectively immutable once published, so handing out `Arc<KeyRecord>`
(share, never deep-clone) is safe. The only cost is an `Arc` clone (atomic refcount bump + pointer copy)
instead of a full deep clone per `verify` call. This is the main win.

> Caveat to flag: `update_key`/`delete` refresh the map with a *new* set of records; any old `Arc<KeyRecord>`
> still held by an in-flight request keeps its (slightly stale) view. That was already true today (callers used
> the owned clone captured at verify time), so semantics are unchanged. For the config/management flows this is
> fine; explicitly note it so reviewers accept the tradeoff.

## Proposed change

### 1. `KeyStore` cache stores `Arc<KeyRecord>` (Source of truth)

`src/auth/keys.rs`:

- Change `cache: RwLock<HashMap<String, KeyRecord>>` → `RwLock<HashMap<String, Arc<KeyRecord>>>`.
- `refresh_cache`: build `Arc::new` per record when populating.
- `insert`: `guard.insert(record.token.clone(), Arc::new(record))` (record is moved in, no clone).
- `verify`: return `Option<Arc<KeyRecord>>`.

```rust
pub fn verify(&self, token: &str, allowed_roles: &[Role]) -> Option<Arc<KeyRecord>> {
    if let Some(record) = self.cache.read().ok().and_then(|g| g.get(token).cloned()) {
        return allowed_roles.contains(&record.role).then_some(record);
    }
    let record = match self.fetch_by_token(token) {
        Ok(Some(r)) => r,
        Ok(None) => return None,
        Err(err) => { log::error(format!("key lookup failed for token={token}: {err}")); return None; }
    };
    if let Ok(mut guard) = self.cache.write() {
        guard.insert(record.token.clone(), Arc::new(record));
    }
    let record = self.cache.read().ok()?.get(token).cloned()?; // re-look-up to share the Arc
    allowed_roles.contains(&record.role).then_some(record)
}
```

(The re-look-up on the miss path ensures we hand out the *same* `Arc` already stored, so subsequent hits
share one allocation. `allowed_roles` check stays; if role not allowed we return `None` but the record is still cached.)

### 2. Call sites — keep `KeyRecord`-typed surface where cheap

Each caller only **reads** fields, never mutates, so `Arc<KeyRecord>` derefs transparently. Options per site,
in order of preference (minimal diff):

| Call site | Current | Change |
|---|---|---|
| `proxy/admission.rs:122` `authorized_key` → `Option<KeyRecord>` | owns clone | Change return to `Option<Arc<KeyRecord>>` (single use in `proxy/server.rs`). Prospects are plumbed as `Option<&KeyRecord>` in planner (already borrowed) |
| `proxy/server.rs:93` `let visible_key = authorized_key(...)` | owned | Becomes `Arc<KeyRecord>`. `.as_ref()` gives `&KeyRecord` for planner (unchanged). Replace `key.name.clone()` with `key.name.clone()` on `&KeyRecord` (already a clone — unchanged) |
| `app/state.rs:46` `authorize_management` | reads `record.role` | change to `Option<Arc<KeyRecord>>`, `.map(|r| r.role)` unchanged via Deref |
| `worker/server.rs:128` `record` | owned clone | `record` becomes `Arc<KeyRecord>`; `record.name.clone()` (line 136) and `record.role.as_str()` (line 191) work via Deref — no change needed except type annotation if any |
| `ClientTask.key_name` (proxy/models/client_task.rs:13) | `Option<String>` | Keep as-is (needs an owned `String` to hand across threads). It's a single small string clone — acceptable; do **not** thread the whole `Arc` into `ClientTask` (would extend record lifetime to end of request for one string). |

### 3. Tests

`src/auth/keys.rs` tests currently call `store.verify(...)` and compare field accesses like
`verified.name`, `verified.whitelist_models`, `assert_eq!(verified.capture, ...)`. With `Arc<KeyRecord>`
these all work via Deref with **no test changes**. `assert_eq!(verified.whitelist_models, vec![...])`
compares `Vec<String>` on both sides (Deref gives `&KeyRecord`) — fine.

Add one test proving we share, not clone:

```rust
#[test]
fn verify_returns_shared_arc_not_a_clone() -> io::Result<()> {
    // ... seed key ...
    let a = store.verify(&token, &[Role::Client]).unwrap();
    let b = store.verify(&token, &[Role::Client]).unwrap();
    assert!(Arc::ptr_eq(&a, &b), "same Arc should be shared across hits");
    Ok(())
}
```

### 4. Keep `id`/`capture` reads as-is

The `RequestContext` construction (`proxy/server.rs:147-150, 185-188`) reads `key.id`, `key.capture`
(cheap `Copy`) and clones `key.name` once per request. `key.id`/`key.capture` are not worth changing;
the `name` clone is one string and necessary for `ClientTask.key_name`. The win is eliminating the
per-request full-key deep clone in `verify` and the redundant re-clones in `insert`.

## Files touched

- `src/auth/keys.rs` — cache type, `Arc` wrapping, `verify` signature/body. **Core change.**
- `src/servers/proxy/admission.rs` — `authorized_key` returns `Option<Arc<KeyRecord>>`.
- `src/servers/proxy/server.rs` — no logic change; type flows through borrows (`visible_key.as_ref()`).
- `src/app/state.rs` — `authorize_management` returns `Option<Arc<KeyRecord>>`.
- `src/servers/worker/server.rs` — none *required* (Deref covers it); verify any explicit type annotations.
- Tests in `src/auth/keys.rs` — add the `Arc::ptr_eq` shared-identity test; existing tests unchanged.

## Verification

- `export PATH="$HOME/.cargo/bin:$PATH"`
- `cargo test` (binary crate — do **not** use `cargo test --lib`).
- `cargo build --release` (clean).
- Confirm no new `Arc<KeyRecord>` regressions in the worker path; worker server logic untouched.

## Out of scope (deliberately not done here)

- #3 (header lowercase `String` allocs), #4 (duplicate JSON body parse), and everything else in the review —
  separate tasks.
- Changing `ClientTask.key_name` to carry an `Arc` — not worth it (single string, extends lifetime).
- Both-fields `id`/`capture` are `Copy`; no change.
