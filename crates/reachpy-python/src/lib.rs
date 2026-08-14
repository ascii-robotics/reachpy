//! Single compiled extension aggregating reachpy-messages and
//! reachpy-runtime as Python submodules. This is what maturin actually
//! builds now -- reachpy-messages and reachpy-runtime stay independent
//! crates (own tests, own `cargo check`), depended on here as libraries.
//!
//! Each of them keeps its own `#[pymodule]` function -- that macro leaves
//! the underlying function callable as a plain Rust function regardless
//! (it only wires up the `PyInit_*` symbol when the crate itself is
//! compiled as the top-level cdylib, which neither is here). We just
//! call each one against a fresh child `PyModule` and attach it.

use pyo3::prelude::*;
use pyo3::types::PyModule;

#[pymodule]
fn _reachpy(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    let messages = PyModule::new(py, "messages")?;
    _reachpy_messages::_reachpy_messages(&messages)?;
    m.add_submodule(&messages)?;

    let runtime = PyModule::new(py, "runtime")?;
    _reachpy_runtime::_reachpy_runtime(&runtime)?;
    m.add_submodule(&runtime)?;

    // Submodules attached via add_submodule() aren't automatically
    // registered in sys.modules -- attribute access (`_reachpy.messages`)
    // works fine without this, but `import reachpy._reachpy.messages` as
    // its own statement, or anything doing introspection via
    // sys.modules, needs the explicit entry. Cheap to do, avoids a sharp
    // edge later.
    let sys_modules = py.import("sys")?.getattr("modules")?;
    sys_modules.set_item("reachpy._reachpy.messages", &messages)?;
    sys_modules.set_item("reachpy._reachpy.runtime", &runtime)?;

    Ok(())
}