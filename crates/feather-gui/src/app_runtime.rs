//! GUI runtimes must not wait indefinitely for DNS/auth or other blocking jobs
//! in Runtime::drop on the native window thread. Active runs get a cooperative
//! cancellation grace period in lifecycle.rs before this final nonblocking drop.
use std::ops::Deref;
use tokio::runtime::Runtime;

pub struct UiRuntime(Option<Runtime>);

impl From<Runtime> for UiRuntime {
    fn from(runtime: Runtime) -> Self {
        Self(Some(runtime))
    }
}

impl Deref for UiRuntime {
    type Target = Runtime;

    fn deref(&self) -> &Runtime {
        self.0.as_ref().expect("runtime alive until drop")
    }
}

impl Drop for UiRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            // Equivalent to shutdown_timeout(Duration::ZERO): unfinished blocking
            // work is detached, not joined on the native UI thread. The main
            // process still exits normally after window/database teardown.
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_returns_while_blocking_job_is_still_waiting() {
        let runtime: UiRuntime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap()
            .into();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (ended_tx, ended_rx) = std::sync::mpsc::channel();
        runtime.spawn_blocking(move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
            let _ = ended_tx.send(());
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(runtime);
            let _ = dropped_tx.send(());
        });
        let dropped = dropped_rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_ok();
        let _ = release_tx.send(());
        ended_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(
            dropped,
            "native UI teardown must not wait on an unrelated blocking job"
        );
    }
}
