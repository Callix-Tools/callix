//! Calls, references and annotations.

pub mod store;

use store::{Engine, Identifier, Labeled};

/// A module-level constant.
pub const DEFAULT_REGION: &str = "eu";

/// Builds an engine and calls a method on it.
pub fn build(region: String) -> Engine {
    let engine = Engine::new(region);
    engine.ping();
    engine
}

/// Takes a trait object and calls through it.
pub fn inspect(item: &dyn Labeled) -> String {
    item.describe()
}

/// Uses the alias in a signature.
pub fn fetch_one(ident: Identifier) -> String {
    format!("{ident}")
}

#[cfg(test)]
mod tests {
    use super::build;

    #[test]
    fn builds() {
        assert!(build(String::from("eu")).ping());
    }
}
