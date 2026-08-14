mod transport;
mod zenoh_transport;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::sync::Arc;
use std::time::Duration;
use transport::NodeHandle;
use zenoh_transport::ZenohTransport;

/// A live ReachPy node. Construction opens a Zenoh session; nothing about
/// ROS2 or CDR is visible from here or below.
#[pyclass]
struct PyNode {
    inner: Arc<NodeHandle<ZenohTransport>>,
}

#[pymethods]
impl PyNode {
    #[new]
    fn new(name: String) -> PyResult<Self> {
        let transport =
            ZenohTransport::open(&name).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(NodeHandle::new(transport)),
        })
    }

    fn create_publisher(&self, topic: String) -> PyPublisher {
        PyPublisher {
            node: self.inner.clone(),
            topic,
        }
    }

    /// `callback` is called with the raw payload bytes for each message.
    /// Schema-aware decoding happens in Python (reachpy.Topic / Message),
    /// not here — this layer only moves bytes.
    ///
    /// Raises if the transport itself rejects the subscription (e.g. a
    /// real network failure) -- this is a real Result now, not silently
    /// logged and dropped.
    fn create_subscription(&self, topic: String, callback: Py<PyAny>) -> PyResult<()> {
        self.inner
            .create_subscription(&topic, move |bytes| {
                Python::attach(|py| {
                    let pybytes = PyBytes::new(py, &bytes);
                    if let Err(e) = callback.call1(py, (pybytes,)) {
                        e.print(py);
                    }
                });
            })
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    fn create_timer(&self, period_secs: f64, callback: Py<PyAny>) {
        self.inner
            .create_timer(Duration::from_secs_f64(period_secs), move || {
                Python::attach(|py| {
                    if let Err(e) = callback.call0(py) {
                        e.print(py);
                    }
                });
            });
    }

    /// Blocks the calling thread while the node's background workers run.
    /// Releases the GIL while waiting (so other Python threads, if any,
    /// stay responsive) and checks for signals periodically so Ctrl+C
    /// still works — same UX as `rclpy.spin()`, no ROS2 underneath.
    fn spin(&self, py: Python<'_>) -> PyResult<()> {
        loop {
            py.check_signals()?;
            py.detach(|| std::thread::sleep(Duration::from_millis(50)));
        }
    }

    /// Stops all background workers/timers and blocks until they've
    /// exited. Call this explicitly before your script ends (same
    /// requirement `rclpy.shutdown()`/`node.destroy_node()` already have
    /// today) — don't rely on the object simply going out of scope.
    /// Rust's Drop timing during CPython interpreter teardown isn't
    /// reliable enough for threads that call back into Python: a worker
    /// can be mid-callback exactly when Py_Finalize runs regardless of
    /// any internal flag, so shutdown needs to complete on its own turn,
    /// before the interpreter starts tearing down.
    ///
    /// Releases the GIL for the duration (py.detach) -- without this, a
    /// worker thread blocked in Python::attach() trying to run its
    /// callback deadlocks against this method holding the GIL while it
    /// blocks on JoinHandle::join(). Two nodes running concurrently makes
    /// this race easy to hit; a single node can get lucky and miss it,
    /// which is exactly how this shipped once already.
    fn destroy_node(&self, py: Python<'_>) {
        py.detach(|| self.inner.shutdown());
    }
}

#[pyclass]
struct PyPublisher {
    node: Arc<NodeHandle<ZenohTransport>>,
    topic: String,
}

#[pymethods]
impl PyPublisher {
    /// Raises if the underlying publish actually fails (real network
    /// error), instead of silently logging it where the caller can never
    /// find out.
    fn publish(&self, data: &[u8]) -> PyResult<()> {
        self.node
            .publish(&self.topic, data.to_vec())
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }
}

#[pymodule]
fn _reachpy_runtime(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyNode>()?;
    m.add_class::<PyPublisher>()?;
    Ok(())
}