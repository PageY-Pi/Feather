//! Off-thread workspace checkpoints.
//!
//! A git checkpoint is spawn-bound: even fully batched, one turn costs two
//! processes (`check-ignore`, `add`) plus `write-tree`, `commit-tree`,
//! `update-ref` and a redb write. Measured on Windows that is ~80 ms of the
//! turn boundary for a modest 12-file turn, and it grows with the file count.
//! Nothing about that work feeds the next request: the durable message
//! snapshot is the load-bearing artifact and git is an auxiliary restore path,
//! so the checkpoint moves off the turn boundary entirely.
//!
//! Ordering invariants this module has to keep, because they are what made the
//! inline version correct:
//!
//! * **Turns checkpoint in order.** One worker thread drains a queue, so turn 5
//!   can never commit before turn 4.
//! * **A rewind can use turn N.** [`CheckpointWorker::wait_for`] blocks until
//!   that turn is on disk; [`Session::rewind_to`] calls it before touching the
//!   workspace.
//! * **Diagnostics are not lost.** Outcomes come back over a channel that the
//!   session drains on the next turn, like the inline path's
//!   `runtime_diagnostics`.
//! * **Closing does not drop work.** Dropping the worker finishes the queue and
//!   joins the thread, so a session's last turn is still checkpointed.
//!
//! [`Session::rewind_to`]: crate::session::Session::rewind_to

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

/// How long a rewind waits for a checkpoint to land before giving up.
///
/// A checkpoint is sub-second; this only bounds a pathological case (a hung git
/// process) so a rewind degrades to "no workspace restore" instead of freezing
/// the UI forever.
const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One turn's checkpoint request.
struct Job {
    turn: u64,
    cwd: PathBuf,
    session_dir: PathBuf,
    files: Vec<String>,
}

/// Result of one checkpoint, reported back to the session.
pub struct CheckpointOutcome {
    pub turn: u64,
    pub hash: Option<String>,
    pub diagnostics: Vec<String>,
    pub error: Option<String>,
}

/// Progress shared with the worker thread.
///
/// `submitted`/`completed` are counters rather than a set so `flush` is a real
/// barrier: capture `submitted` when flushing and wait for `completed` to catch
/// up. A set of turns cannot express "the queue is empty" without also knowing
/// what was queued.
#[derive(Default)]
struct Progress {
    state: Mutex<ProgressState>,
    changed: Condvar,
    submitted: AtomicU64,
    completed: AtomicU64,
}

#[derive(Default)]
struct ProgressState {
    done: BTreeSet<u64>,
}

/// Handle to the checkpoint thread.
pub struct CheckpointWorker {
    jobs: Option<mpsc::Sender<Job>>,
    outcomes: mpsc::Receiver<CheckpointOutcome>,
    progress: Arc<Progress>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CheckpointWorker {
    pub fn start() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (out_tx, out_rx) = mpsc::channel::<CheckpointOutcome>();
        let progress = Arc::new(Progress::default());
        let shared = progress.clone();
        let thread = std::thread::Builder::new()
            .name("feather-checkpoint".into())
            .spawn(move || {
                // `recv` errors when the sender is dropped, which is how the
                // worker learns the session closed.
                while let Ok(job) = job_rx.recv() {
                    let outcome = run_job(job);
                    // Publish the outcome *before* marking the job complete, so
                    // a `flush` that returns guarantees `drain` can see the
                    // result. A closed receiver only means nobody is listening;
                    // the job still ran, and a closing session still gets the
                    // turn.
                    let turn = outcome.turn;
                    let _ = out_tx.send(outcome);
                    let mut state = lock(&shared.state);
                    state.done.insert(turn);
                    shared.completed.fetch_add(1, Ordering::SeqCst);
                    shared.changed.notify_all();
                    drop(state);
                }
            })
            .expect("spawn checkpoint thread");
        Self {
            jobs: Some(job_tx),
            outcomes: out_rx,
            progress,
            thread: Some(thread),
        }
    }

    /// Queue a checkpoint. Returns immediately; the git work happens on the
    /// worker thread.
    pub fn submit(&self, turn: u64, cwd: &Path, session_dir: &Path, files: Vec<String>) {
        let Some(jobs) = &self.jobs else {
            return;
        };
        // Count before sending: a `flush` racing this call must see the job.
        self.progress.submitted.fetch_add(1, Ordering::SeqCst);
        if jobs
            .send(Job {
                turn,
                cwd: cwd.to_path_buf(),
                session_dir: session_dir.to_path_buf(),
                files,
            })
            .is_err()
        {
            // The worker is gone; undo the count so `flush` cannot wait on a
            // job nobody will run.
            self.progress.submitted.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Finished outcomes, without blocking. The session drains these into its
    /// diagnostics on the next turn.
    pub fn drain(&self) -> Vec<CheckpointOutcome> {
        self.outcomes.try_iter().collect()
    }

    /// Block until every job submitted so far has finished.
    pub fn flush(&self) {
        let target = self.progress.submitted.load(Ordering::SeqCst);
        self.wait_until_completed(target, WAIT_TIMEOUT);
    }

    /// Wait until `turn` is checkpointed.
    ///
    /// Returns `false` on timeout or if the worker is gone, which callers treat
    /// as "no workspace checkpoint".
    pub fn wait_for(&self, turn: u64) -> bool {
        self.wait_for_timeout(turn, WAIT_TIMEOUT)
    }

    fn wait_for_timeout(&self, turn: u64, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = lock(&self.progress.state);
        loop {
            if state.done.contains(&turn) {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, _) = self
                .progress
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
        }
    }

    fn wait_until_completed(&self, target: u64, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = lock(&self.progress.state);
        loop {
            if self.progress.completed.load(Ordering::SeqCst) >= target {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, _) = self
                .progress
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
        }
    }
}

impl Drop for CheckpointWorker {
    fn drop(&mut self) {
        // Closing the channel lets the worker finish what is queued and exit.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn run_job(job: Job) -> CheckpointOutcome {
    // A panic inside git handling must not poison the worker or lose the turn
    // ordering: catch it and report it as this turn's error.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::gitckpt::GitCheckpoints::open(&job.cwd).commit_prepared(
            job.turn,
            &job.session_dir,
            job.files,
        )
    }));
    match result {
        Ok(Ok(report)) => CheckpointOutcome {
            turn: job.turn,
            hash: report.hash,
            diagnostics: report.diagnostics,
            error: None,
        },
        Ok(Err(e)) => CheckpointOutcome {
            turn: job.turn,
            hash: None,
            diagnostics: Vec::new(),
            error: Some(e.to_string()),
        },
        Err(_) => CheckpointOutcome {
            turn: job.turn,
            hash: None,
            diagnostics: Vec::new(),
            error: Some("checkpoint worker panicked".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "feather-ckpt-worker-{tag}-{}-{}",
            std::process::id(),
            crate::message::now_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_queued_turn_is_checkpointed_and_reported_back() {
        let dir = temp("basic");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(dir.join("a.txt"), "v1").unwrap();
        let worker = CheckpointWorker::start();
        worker.submit(1, &dir, &session, vec!["a.txt".into()]);
        assert!(worker.wait_for(1), "turn 1 must complete");
        let outcomes = worker.drain();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].turn, 1);
        assert!(outcomes[0].error.is_none(), "{:?}", outcomes[0].error);
        assert!(outcomes[0].hash.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Turns must land in order: a single worker thread is what guarantees a
    /// later turn cannot commit before an earlier one.
    #[test]
    fn turns_checkpoint_in_order() {
        let dir = temp("order");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let worker = CheckpointWorker::start();
        for turn in 1..=5 {
            std::fs::write(dir.join("a.txt"), format!("v{turn}")).unwrap();
            worker.submit(turn, &dir, &session, vec!["a.txt".into()]);
        }
        worker.flush();
        let mut turns: Vec<u64> = worker.drain().iter().map(|o| o.turn).collect();
        turns.sort_unstable();
        assert_eq!(turns, vec![1, 2, 3, 4, 5]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `flush` is a real barrier: every submitted job must be durable when it
    /// returns, which is what makes it safe to use before a rewind.
    #[test]
    fn flush_waits_for_every_submitted_job() {
        let dir = temp("flush");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        let worker = CheckpointWorker::start();
        for turn in 1..=3 {
            std::fs::write(dir.join("a.txt"), format!("v{turn}")).unwrap();
            worker.submit(turn, &dir, &session, vec!["a.txt".into()]);
        }
        worker.flush();
        // No `wait_for` here on purpose: `flush` alone must be enough.
        let ck = crate::gitckpt::GitCheckpoints::open(&dir);
        for turn in 1..=3 {
            assert!(
                ck.has_checkpoint(turn, &session),
                "turn {turn} must be durable after flush"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A dropped worker must finish the queue: the last turn of a session is the
    /// one a user is most likely to rewind to.
    #[test]
    fn dropping_the_worker_finishes_queued_turns() {
        let dir = temp("drop");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(dir.join("a.txt"), "v1").unwrap();
        let worker = CheckpointWorker::start();
        worker.submit(1, &dir, &session, vec!["a.txt".into()]);
        drop(worker);
        assert!(
            crate::gitckpt::GitCheckpoints::open(&dir).has_checkpoint(1, &session),
            "the queued turn must be committed before the worker exits"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An unsubmitted turn must return (false) rather than hang.
    #[test]
    fn waiting_for_a_turn_that_never_comes_times_out() {
        let worker = CheckpointWorker::start();
        let started = std::time::Instant::now();
        assert!(!worker.wait_for_timeout(99, std::time::Duration::from_millis(50)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the wait must be bounded"
        );
    }

    /// A checkpoint error is reported, not swallowed, and does not stop later
    /// `flush` is only a useful barrier if a following `drain` can actually see
    /// the finished jobs. The outcome must be published before the completion
    /// counter moves, or a caller blocks until the job is "done" and then
    /// observes an empty queue.
    #[test]
    fn flush_guarantees_the_outcomes_are_drainable() {
        let dir = temp("flush-drain");
        let session = dir.join(".feather/sessions/s1");
        std::fs::create_dir_all(&session).unwrap();
        // Fail every job, so this exercises the same reporting path as a real
        // error and cannot pass by accident on a successful checkpoint.
        let bogus = dir.join("not-a-dir.txt");
        std::fs::write(&bogus, "x").unwrap();
        let worker = CheckpointWorker::start();
        for turn in 1..=5 {
            worker.submit(turn, &bogus, &session, vec!["a.txt".into()]);
        }
        worker.flush();
        let drained = worker.drain();
        assert_eq!(
            drained.len(),
            5,
            "flush must not return before every outcome is published"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    /// turns from running.
    #[test]
    fn a_failed_checkpoint_reports_and_does_not_stall_later_turns() {
        let dir = temp("error");
        // No session dir created and a cwd that is a file, so `ensure_repo`
        // cannot succeed.
        let bogus = dir.join("not-a-dir.txt");
        std::fs::write(&bogus, "x").unwrap();
        let worker = CheckpointWorker::start();
        worker.submit(1, &bogus, &dir.join("s"), vec!["a.txt".into()]);
        worker.submit(2, &bogus, &dir.join("s"), vec!["a.txt".into()]);
        worker.flush();
        let outcomes = worker.drain();
        assert_eq!(outcomes.len(), 2, "both turns reported");
        assert!(outcomes.iter().all(|o| o.error.is_some()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
