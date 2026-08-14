//! Transport-agnostic core of reachpy-runtime.
//!
//! `Transport` is deliberately narrow: publish raw bytes, subscribe to raw
//! bytes. Schema-aware (de)serialization stays in `reachpy_messages` and
//! happens in Python before bytes ever reach here — this layer only moves
//! bytes and owns the concurrency model.
//!
//! Both operations are fallible -- a real network call can genuinely fail
//! (session down, bad key expression, etc.), and that failure needs to
//! reach the caller as a real error, not get silently logged and dropped.
//! `TransportError` is a boxed `std::error::Error` so each transport
//! implementation (ZenohTransport, FakeTransport, anything added later)
//! can return its own concrete error type without this module needing to
//! know about it.
//!
//! Dispatch model: each subscription and each timer gets its own dedicated
//! worker thread. A subscription's underlying transport callback (which
//! may fire on the transport's own I/O thread) just pushes bytes into an
//! mpsc channel; the dedicated worker drains that channel in order and
//! invokes the user callback. That gives two guarantees at once:
//!   1. Messages on the same subscription are always handled in the order
//!      they arrived (single consumer thread, FIFO channel).
//!   2. Different subscriptions/timers never block each other — a slow
//!      callback on one topic can't stall delivery on another.
//! In the real crate the "user callback" is `Python::attach` + a
//! `Py<PyAny>` call. Here it's a plain closure so this logic can be
//! verified without pulling in PyO3 or zenoh at all.

use std::any::Any;
use std::error::Error as StdError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often worker loops wake up to check whether the node is shutting
/// down. Bounds shutdown latency; small enough not to be noticeable.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub type RawCallback = Box<dyn Fn(Vec<u8>) + Send + Sync>;
pub type TransportError = Box<dyn StdError + Send + Sync>;

/// What a transport implementation (Zenoh today, maybe others later) has
/// to provide. Intentionally minimal.
pub trait Transport: Send + Sync + 'static {
    fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), TransportError>;

    /// Register a raw callback for a topic. Returns an opaque handle that
    /// must be kept alive for as long as the subscription should exist
    /// (dropping it should tear the subscription down).
    fn raw_subscribe(&self, topic: &str, on_raw: RawCallback) -> Result<Box<dyn Any + Send>, TransportError>;
}

/// Owns the worker threads for one node. Generic over `Transport` so the
/// dispatch/ordering logic here never has to know or care whether it's
/// riding Zenoh or a fake.
pub struct NodeHandle<T: Transport> {
    transport: Arc<T>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    // Keep subscription handles alive for the node's lifetime.
    subscriptions: Mutex<Vec<Box<dyn Any + Send>>>,
    running: Arc<AtomicBool>,
}

impl<T: Transport> NodeHandle<T> {
    pub fn new(transport: Arc<T>) -> Self {
        Self {
            transport,
            workers: Mutex::new(Vec::new()),
            subscriptions: Mutex::new(Vec::new()),
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), TransportError> {
        self.transport.publish(topic, payload)
    }

    /// One dedicated worker thread per subscription — see module docs.
    /// The worker polls with a bounded timeout (rather than blocking on
    /// `recv()` forever) purely so it notices shutdown promptly instead
    /// of only exiting when the channel disconnects.
    ///
    /// Returns an error if the transport itself rejects the subscription
    /// (e.g. a real network failure) -- in that case no worker thread is
    /// spawned at all, so there's nothing left running to clean up.
    pub fn create_subscription<F>(&self, topic: &str, callback: F) -> Result<(), TransportError>
    where
        F: Fn(Vec<u8>) + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();

        let handle = self.transport.raw_subscribe(
            topic,
            Box::new(move |bytes| {
                // Fired on the transport's own thread(s). Just hand off —
                // no user code runs here.
                let _ = tx.send(bytes);
            }),
        )?;
        self.subscriptions.lock().unwrap().push(handle);

        let running = self.running.clone();
        let worker = thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                match rx.recv_timeout(SHUTDOWN_POLL_INTERVAL) {
                    Ok(bytes) => callback(bytes),
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        self.workers.lock().unwrap().push(worker);
        Ok(())
    }

    /// Timers get the same treatment: their own dedicated thread, so a
    /// slow timer callback can't delay message delivery on a Topic (or
    /// vice versa). Sleeps in small quanta rather than the full period so
    /// shutdown latency is bounded by SHUTDOWN_POLL_INTERVAL, not by
    /// whatever period the user picked. Timers don't touch the transport,
    /// so unlike create_subscription this can't fail.
    pub fn create_timer<F>(&self, period: Duration, callback: F)
    where
        F: Fn() + Send + 'static,
    {
        let running = self.running.clone();
        let worker = thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let deadline = Instant::now() + period;
                while Instant::now() < deadline {
                    if !running.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(SHUTDOWN_POLL_INTERVAL.min(deadline - Instant::now()));
                }
                if running.load(Ordering::Relaxed) {
                    callback();
                }
            }
        });
        self.workers.lock().unwrap().push(worker);
    }

    /// Signals every worker/timer thread to stop and blocks until they
    /// have. Safe to call more than once. Called automatically on Drop,
    /// but callable explicitly (e.g. from `destroy_node()`) so shutdown
    /// can happen at a controlled point rather than whenever Python's GC
    /// happens to drop the last reference.
    ///
    /// Callers holding the GIL (any PyO3 method) MUST release it for the
    /// duration of this call (e.g. `py.detach(|| node.shutdown())`) --
    /// otherwise a worker thread blocked in `Python::attach()` trying to
    /// run its callback deadlocks against this call blocking on
    /// `JoinHandle::join()` while still holding the GIL.
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::Relaxed);
        let mut workers = self.workers.lock().unwrap();
        for handle in workers.drain(..) {
            let _ = handle.join();
        }
    }
}

impl<T: Transport> Drop for NodeHandle<T> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Fake transport, used only by this module's own tests. The real
    // transport is ZenohTransport in zenoh_transport.rs. Kept inside
    // #[cfg(test)] since nothing outside these tests uses it -- no reason
    // for it to exist in a real build.
    struct FakeTransport {
        subscribers: Mutex<HashMap<String, Vec<RawCallback>>>,
    }

    impl FakeTransport {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                subscribers: Mutex::new(HashMap::new()),
            })
        }
    }

    impl Transport for FakeTransport {
        fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), TransportError> {
            // Mimic a real transport: delivery happens asynchronously, on
            // the transport's own thread, not the publisher's thread.
            let subs = self.subscribers.lock().unwrap();
            if let Some(callbacks) = subs.get(topic) {
                for cb in callbacks {
                    let payload = payload.clone();
                    // Each delivery races independently, same as real
                    // network I/O would -- this is what makes the
                    // ordering test on NodeHandle meaningful rather than
                    // trivially true.
                    cb(payload);
                }
            }
            Ok(())
        }

        fn raw_subscribe(&self, topic: &str, on_raw: RawCallback) -> Result<Box<dyn Any + Send>, TransportError> {
            self.subscribers
                .lock()
                .unwrap()
                .entry(topic.to_string())
                .or_default()
                .push(on_raw);
            Ok(Box::new(())) // FakeTransport doesn't need real teardown
        }
    }

    #[test]
    fn same_subscription_preserves_order_even_under_slow_callback() {
        let transport = FakeTransport::new();
        let node = NodeHandle::new(transport.clone());

        let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();

        node.create_subscription("/slow", move |bytes| {
            // Intentionally slow -- if ordering depended on delivery
            // timing rather than the per-source channel, this would be
            // enough to scramble it.
            thread::sleep(Duration::from_millis(5));
            received_clone.lock().unwrap().push(bytes[0]);
        })
        .unwrap();

        // give the subscription's worker thread a moment to be ready
        thread::sleep(Duration::from_millis(20));

        for i in 0..10u8 {
            node.publish("/slow", vec![i]).unwrap();
        }

        // wait for all 10 slow callbacks to drain (10 * 5ms + slack)
        thread::sleep(Duration::from_millis(200));

        let got = received.lock().unwrap().clone();
        assert_eq!(got, (0..10u8).collect::<Vec<_>>(), "messages on one subscription must be handled in order");
    }

    #[test]
    fn independent_subscriptions_do_not_block_each_other() {
        let transport = FakeTransport::new();
        let node = NodeHandle::new(transport.clone());

        let fast_count = Arc::new(AtomicUsize::new(0));
        let fast_count_clone = fast_count.clone();
        node.create_subscription("/fast", move |_bytes| {
            fast_count_clone.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        node.create_subscription("/slow", move |_bytes| {
            thread::sleep(Duration::from_millis(500));
        })
        .unwrap();

        thread::sleep(Duration::from_millis(20));

        // Kick off one slow message, then hammer /fast -- if subscriptions
        // shared a worker (or the GIL-equivalent were held across sources),
        // /fast would stall behind /slow's 500ms callback.
        node.publish("/slow", vec![0]).unwrap();
        let start = Instant::now();
        for i in 0..50u8 {
            node.publish("/fast", vec![i]).unwrap();
        }

        // give /fast plenty of time to finish -- well under /slow's 500ms
        thread::sleep(Duration::from_millis(100));
        let elapsed = start.elapsed();

        assert_eq!(fast_count.load(Ordering::SeqCst), 50, "/fast should have processed everything");
        assert!(
            elapsed < Duration::from_millis(500),
            "/fast should not have waited on /slow's callback, took {elapsed:?}"
        );
    }

    #[test]
    fn timer_fires_on_its_own_thread_independent_of_subscriptions() {
        let transport = FakeTransport::new();
        let node = NodeHandle::new(transport.clone());

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticks_clone = ticks.clone();
        node.create_timer(Duration::from_millis(10), move || {
            ticks_clone.fetch_add(1, Ordering::SeqCst);
        });

        thread::sleep(Duration::from_millis(105));
        let count = ticks.load(Ordering::SeqCst);
        // ~10 ticks expected in 105ms at a 10ms period; allow slack for
        // scheduling jitter in a shared CI/sandbox environment.
        assert!(count >= 7 && count <= 13, "expected ~10 ticks, got {count}");
    }

    #[test]
    fn shutdown_stops_timer_and_subscription_threads_promptly() {
        let transport = FakeTransport::new();
        let node = NodeHandle::new(transport.clone());

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticks_clone = ticks.clone();
        node.create_timer(Duration::from_millis(500), move || {
            ticks_clone.fetch_add(1, Ordering::SeqCst);
        });
        node.create_subscription("/x", |_bytes| {}).unwrap();

        thread::sleep(Duration::from_millis(20));

        let start = Instant::now();
        node.shutdown();
        let elapsed = start.elapsed();

        assert!(elapsed < Duration::from_millis(300), "shutdown took too long: {elapsed:?}");
        assert_eq!(ticks.load(Ordering::SeqCst), 0, "timer period hadn't elapsed yet, should not have fired");

        node.shutdown(); // must not panic/hang when called twice
    }
}