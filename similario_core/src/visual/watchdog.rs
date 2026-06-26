//! Single background watcher thread shared by every file's frame extraction.
//!
//! Lazily started on first use and never joined - it is a daemon thread that
//! lives for the process lifetime, polling whatever ffmpeg child processes
//! are currently registered (one entry per concurrently-running ffmpeg call,
//! e.g. one per rayon worker thread) and killing any that overrun their
//! deadline or whose stop flag fires. This keeps thread count constant at 1
//! regardless of how many files or windows-per-file are being processed.

use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Why the watchdog killed a registered child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KillReason {
    Stopped,
    TimedOut,
}

struct Entry {
    child: Arc<Mutex<Child>>,
    deadline: Instant,
    stop_flag: Arc<AtomicBool>,
    outcome: Arc<Mutex<Option<KillReason>>>,
}

fn registry() -> &'static Arc<Mutex<Vec<Entry>>> {
    static REGISTRY: OnceLock<Arc<Mutex<Vec<Entry>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let registry = Arc::new(Mutex::new(Vec::new()));
        let background = Arc::clone(&registry);
        std::thread::spawn(move || sweep_loop(&background));
        registry
    })
}

fn sweep_loop(registry: &Mutex<Vec<Entry>>) {
    loop {
        std::thread::sleep(POLL_INTERVAL);

        let mut entries = registry.lock().expect("watchdog registry poisoned");
        entries.retain(|entry| {
            let reason = if entry.stop_flag.load(Ordering::Relaxed) {
                Some(KillReason::Stopped)
            } else if Instant::now() >= entry.deadline {
                Some(KillReason::TimedOut)
            } else {
                None
            };
            let Some(reason) = reason else { return true };

            let _ = entry.child.lock().expect("watchdog mutex poisoned").kill();
            *entry.outcome.lock().expect("watchdog mutex poisoned") = Some(reason);
            false
        });
    }
}

/// RAII handle for one ffmpeg child registered with the global watchdog.
/// Unregisters itself on drop, so callers don't need to remember to clean up
/// on early-return error paths.
pub(crate) struct Watched {
    child: Arc<Mutex<Child>>,
    outcome: Arc<Mutex<Option<KillReason>>>,
}

impl Watched {
    pub(crate) fn child(&self) -> &Arc<Mutex<Child>> {
        &self.child
    }

    /// Why the watchdog killed this child, if it did.
    pub(crate) fn outcome(&self) -> Option<KillReason> {
        *self.outcome.lock().expect("watchdog mutex poisoned")
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        registry()
            .lock()
            .expect("watchdog registry poisoned")
            .retain(|e| !Arc::ptr_eq(&e.child, &self.child));
    }
}

/// Registers a freshly spawned ffmpeg child with the single process-wide
/// watchdog thread. It is killed if `timeout` elapses or `stop_flag` fires
/// before the returned [`Watched`] is dropped.
pub(crate) fn watch(child: Child, timeout: Duration, stop_flag: &Arc<AtomicBool>) -> Watched {
    let child = Arc::new(Mutex::new(child));
    let outcome = Arc::new(Mutex::new(None));
    let entry = Entry {
        child: Arc::clone(&child),
        deadline: Instant::now() + timeout,
        stop_flag: Arc::clone(stop_flag),
        outcome: Arc::clone(&outcome),
    };
    registry().lock().expect("watchdog registry poisoned").push(entry);
    Watched { child, outcome }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn spawn_sleep(secs: u64) -> Child {
        Command::new("sleep")
            .arg(secs.to_string())
            .spawn()
            .expect("sleep binary available on test host")
    }

    #[test]
    fn kills_child_on_timeout() {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let watched = watch(spawn_sleep(30), Duration::from_millis(200), &stop_flag);

        std::thread::sleep(Duration::from_millis(600));

        assert_eq!(watched.outcome(), Some(KillReason::TimedOut));
        let status = watched
            .child()
            .lock()
            .expect("test mutex poisoned")
            .wait()
            .expect("wait on killed child");
        assert!(!status.success());
    }

    #[test]
    fn kills_child_on_stop_flag() {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let watched = watch(spawn_sleep(30), Duration::from_secs(30), &stop_flag);

        stop_flag.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(400));

        assert_eq!(watched.outcome(), Some(KillReason::Stopped));
    }

    #[test]
    fn leaves_child_alone_when_it_exits_in_time() {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let watched = watch(spawn_sleep(0), Duration::from_secs(30), &stop_flag);

        let status = watched
            .child()
            .lock()
            .expect("test mutex poisoned")
            .wait()
            .expect("wait on exited child");

        assert!(status.success());
        assert_eq!(watched.outcome(), None);
    }

    #[test]
    fn multiple_concurrent_children_tracked_independently() {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let short_lived = watch(spawn_sleep(0), Duration::from_secs(30), &stop_flag);
        let stuck = watch(spawn_sleep(30), Duration::from_millis(200), &stop_flag);

        let status = short_lived
            .child()
            .lock()
            .expect("test mutex poisoned")
            .wait()
            .expect("wait on exited child");
        std::thread::sleep(Duration::from_millis(600));

        assert!(status.success());
        assert_eq!(short_lived.outcome(), None);
        assert_eq!(stuck.outcome(), Some(KillReason::TimedOut));
    }
}
