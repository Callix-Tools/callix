//! Types the service annotates against.

/// An alias the service uses in signatures.
pub type Identifier = u32;

/// Implemented by Engine.
pub trait Labeled {
    fn describe(&self) -> String;
}

/// Carries a field.
pub struct Base {
    pub label: String,
}

impl Labeled for Base {
    fn describe(&self) -> String {
        self.label.clone()
    }
}

/// Embeds a base and adds a field.
pub struct Engine {
    pub base: Base,
    pub region: String,
}

impl Engine {
    pub fn new(region: String) -> Self {
        Self { base: Base { label: String::new() }, region }
    }

    pub fn ping(&self) -> bool {
        !self.region.is_empty()
    }
}
