//! The binary links `libpython` dynamically. When the interpreter it was built
//! against keeps that library outside the loader's default search path (a uv or
//! pyenv Python, say), record the directory as an rpath so the binary — and the
//! tests that run it — start without `LD_LIBRARY_PATH`.

fn main() {
    pyo3_build_config::add_libpython_rpath_link_args();
}
