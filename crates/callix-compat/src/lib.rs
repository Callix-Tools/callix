//! The PyO3 attribute macros, as no-ops.
//!
//! callix is a Python extension module first, and a native command line second.
//! The command line must not link `libpython`, but the adapters it runs are
//! written as PyO3 classes. In a build without the `python` feature these
//! attributes stand in for the real ones: each returns the item it was given,
//! minus the helper attributes (`#[new]`, `#[getter]`, `#[pyo3(...)]`, …) that
//! only PyO3 understands. What was a `#[pymethods]` block becomes an ordinary
//! `impl` whose methods the native code calls directly.
//!
//! With the feature on, `crate::py` re-exports PyO3's own macros and this crate
//! is not used for anything.

use proc_macro::TokenStream;
use quote::ToTokens;
use syn::visit_mut::{self, VisitMut};
use syn::{Attribute, Item};

/// Helper attributes PyO3 and pyo3-stub-gen put inside classes, impl blocks and
/// signatures. Without PyO3 nothing defines them, so they must not survive.
const INERT: [&str; 12] = [
    "pyo3", "new", "getter", "setter", "staticmethod", "classmethod", "classattr", "pyclass",
    "pymethods", "pyfunction", "gen_stub", "args",
];

struct Strip;

impl Strip {
    fn is_inert(attr: &Attribute) -> bool {
        attr.path().get_ident().is_some_and(|ident| INERT.iter().any(|name| ident == name))
    }
}

impl VisitMut for Strip {
    fn visit_attributes_mut(&mut self, attrs: &mut Vec<Attribute>) {
        attrs.retain(|attr| !Self::is_inert(attr));
    }

    fn visit_item_mut(&mut self, item: &mut Item) {
        visit_mut::visit_item_mut(self, item);
    }
}

fn passthrough(item: TokenStream) -> TokenStream {
    let mut parsed: syn::File = match syn::parse::<syn::File>(item.clone()) {
        Ok(file) => file,
        // Not something we understand (a macro invocation in item position,
        // say): hand it back untouched rather than invent an error.
        Err(_) => return item,
    };
    Strip.visit_file_mut(&mut parsed);
    parsed.into_token_stream().into()
}

macro_rules! noop_attribute {
    ($($name:ident),* $(,)?) => {$(
        #[proc_macro_attribute]
        pub fn $name(_args: TokenStream, item: TokenStream) -> TokenStream {
            passthrough(item)
        }
    )*};
}

noop_attribute!(
    pyclass,
    pymethods,
    pyfunction,
    pymodule,
    gen_stub_pyclass,
    gen_stub_pyclass_enum,
    gen_stub_pymethods,
    gen_stub_pyfunction,
);
