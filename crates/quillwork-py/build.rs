//! `cargo test` links libpython into the test binary; on macOS the framework Python needs an
//! rpath to load. No-op for maturin builds (extension modules do not link libpython).
fn main() {
    pyo3_build_config::add_libpython_rpath_link_args();
    pyo3_build_config::add_python_framework_link_args();
}
