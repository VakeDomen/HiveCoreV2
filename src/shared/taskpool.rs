//! A growable, thread-reusing task pool.
//!
//! Unlike a fixed-bounded pool or a fresh thread per task, this pool keeps a set of
//! evergreen "core" workers parked on a job queue and reuses them across submissions.
//! When all workers are busy, a new worker is spawned on demand (unbounded growth),
//! and workers that sit idle for a grace period retire, so the pool decays back toward
//! the core size under low load.
//!
//! The job queue is a `Mutex<VecDeque>` + `Condvar` (rather than `mpsc`) so that a worker
//! about to retire can atomically re-check the queue: a job that arrives in the same
//! instant is taken and run instead of being stranded.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Default how long an idle (non-core) worker waits before retiring (decay rate).
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

struct PoolState {
    queue: VecDeque<Job>,
    /// Number of worker threads currently alive (born minus retired).
    workers: usize,
    /// Number of workers currently parked, blocked on the condvar waiting for work.
    idle: usize,
}

impl PoolState {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            workers: 0,
            idle: 0,
        }
    }
}

/// A growable pool that reuses idle threads and spawns on demand under bursts.
pub struct GrowablePool {
    state: Arc<Mutex<PoolState>>,
    notifier: Arc<Condvar>,
    core: usize,
    stack_kb: usize,
    idle_timeout: Duration,
}

impl GrowablePool {
    /// Create a pool with `core` evergreen workers and an idle timeout of 10s.
    /// `stack_kb` sets the worker stack size.
    pub fn new(core: usize, stack_kb: usize) -> Self {
        Self::with_idle_timeout(core, stack_kb, DEFAULT_IDLE_TIMEOUT)
    }

    /// Same as [`new`], but with a custom idle timeout (used by tests to keep them fast).
    pub(crate) fn with_idle_timeout(core: usize, stack_kb: usize, idle_timeout: Duration) -> Self {
        let pool = Self {
            state: Arc::new(Mutex::new(PoolState::new())),
            notifier: Arc::new(Condvar::new()),
            core: core.max(1),
            stack_kb: stack_kb.max(64),
            idle_timeout,
        };
        for _ in 0..core {
            pool.spawn_worker();
        }
        pool
    }

    /// Submit a job to run on a pooled (or freshly grown) worker.
    pub fn spawn<F>(&self, job: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let mut state = self.state.lock().expect("taskpool state poisoned");
        state.queue.push_back(Box::new(job));
        self.try_handoff(&mut state);
    }

    /// Number of live workers (informational / tests).
    pub fn worker_count(&self) -> usize {
        self.state.lock().expect("taskpool state poisoned").workers
    }

    fn try_handoff(&self, state: &mut PoolState) {
        if state.idle > 0 {
            // Wake one parked worker; it will pop the job.
            self.notifier.notify_one();
            return;
        }
        // All workers busy. Grow on demand, but never spawn more workers than there are
        // outstanding queued jobs, which prevents a pathological flood from creating an
        // unbounded cascade while still allowing concurrency to track burst depth.
        let active = state.workers - state.idle;
        if active < state.queue.len() {
            self.spawn_worker_locked(state);
        } else {
            self.notifier.notify_one();
        }
    }

    fn spawn_worker(&self) {
        let mut state = self.state.lock().expect("taskpool state poisoned");
        self.spawn_worker_locked(&mut state);
    }

    fn spawn_worker_locked(&self, state: &mut PoolState) {
        state.workers += 1;
        let pool_state = Arc::clone(&self.state);
        let notifier = Arc::clone(&self.notifier);
        let name = if state.workers > self.core {
            "hive-task".to_string()
        } else {
            "hive-task-core".to_string()
        };
        let stack_kb = self.stack_kb;
        let idle_timeout = self.idle_timeout;
        thread::Builder::new()
            .name(name)
            .stack_size(stack_kb * 1024)
            .spawn(move || worker_loop(pool_state, notifier, idle_timeout))
            .expect("failed to spawn taskpool worker");
    }
}

fn worker_loop(pool_state: Arc<Mutex<PoolState>>, notifier: Arc<Condvar>, idle_timeout: Duration) {
    loop {
        let job = {
            let mut state = pool_state.lock().expect("taskpool state poisoned");
            if let Some(job) = state.queue.pop_front() {
                // Work already waiting: take it immediately without parking.
                (Some(job), false)
            } else {
                // No work available: park as an idle worker until a job arrives or the
                // idle timeout elapses. Loop to handle spurious wakes.
                state.idle += 1;
                loop {
                    let (guard, timeout) = notifier
                        .wait_timeout(state, idle_timeout)
                        .unwrap_or_else(|e| e.into_inner());
                    state = guard;

                    if let Some(job) = state.queue.pop_front() {
                        // A job arrived: we remain the active worker and run it.
                        state.idle -= 1;
                        break (Some(job), false);
                    }
                    if timeout.timed_out() && state.workers > 1 {
                        // Genuinely idle for the whole grace period and we are not the
                        // last survivor: retire without stranding the pool.
                        state.idle -= 1;
                        state.workers -= 1;
                        break (None, true);
                    }
                    // Spurious wake with no job (and not idle-expired): wait again,
                    // still counted as idle.
                }
            }
        };

        let (job, retired) = job;
        if retired {
            return;
        }
        if let Some(job) = job {
            // Run outside the lock so other workers are not blocked.
            job();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn test_pool() -> GrowablePool {
        GrowablePool::with_idle_timeout(2, 256, Duration::from_millis(100))
    }

    #[test]
    fn runs_all_jobs() {
        let pool = test_pool();
        let (tx, rx) = mpsc::channel();
        for i in 0..100 {
            let tx = tx.clone();
            pool.spawn(move || tx.send(i).unwrap());
        }
        drop(tx);
        let got: Vec<i32> = rx.iter().collect();
        assert_eq!(got.len(), 100);
        assert_eq!(got.iter().sum::<i32>(), (0..100).sum::<i32>());
    }

    #[test]
    fn grows_beyond_core_under_burst() {
        let pool = GrowablePool::with_idle_timeout(1, 256, Duration::from_millis(100));
        let (tx, rx) = mpsc::channel();
        // 8 concurrent blocking jobs exceed the single core worker, forcing growth.
        for _ in 0..8 {
            let tx = tx.clone();
            pool.spawn(move || {
                tx.send(()).unwrap();
                thread::sleep(Duration::from_millis(100));
            });
        }
        for _ in 0..8 {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
        thread::sleep(Duration::from_millis(50));
        assert!(
            pool.worker_count() >= 2,
            "expected growth, got {} workers",
            pool.worker_count()
        );
    }

    #[test]
    fn reuses_threads_for_sequential_jobs() {
        let pool = test_pool();
        // Many sequential quick jobs: the core workers should handle them all without
        // growing the worker count beyond the core crew.
        let (tx, rx) = mpsc::channel();
        for _ in 0..50 {
            let tx = tx.clone();
            pool.spawn(move || tx.send(()).unwrap());
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
        assert!(
            pool.worker_count() <= 2,
            "expected reuse, got {} workers",
            pool.worker_count()
        );
    }

    #[test]
    fn decays_back_toward_core_after_idle() {
        let pool = GrowablePool::with_idle_timeout(1, 256, Duration::from_millis(80));
        // Grow the pool with a burst of concurrent work.
        let (tx, rx) = mpsc::channel();
        for _ in 0..8 {
            let tx = tx.clone();
            pool.spawn(move || {
                thread::sleep(Duration::from_millis(30));
                tx.send(()).unwrap();
            });
        }
        for _ in 0..8 {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
        thread::sleep(Duration::from_millis(50));
        let grown = pool.worker_count();
        assert!(grown >= 2, "expected growth before decay, got {grown}");

        // Wait for the extra workers' idle timeout to elapse so they retire.
        thread::sleep(Duration::from_millis(300));
        let decayed = pool.worker_count();
        assert!(
            decayed < grown,
            "expected decay from {grown}, got {decayed}"
        );
    }
}
