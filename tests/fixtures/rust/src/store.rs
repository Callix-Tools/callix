//! Types the service annotates against.

/// An alias the service uses in signatures.
pub type Identifier = u32;

/// Implemented by Engine.
pub trait Labeled {
    type Tag;
    const KIND: u32 = 1;
    fn describe(&self) -> String;
}

/// Carries a field.
pub struct Base {
    pub label: String,
}

impl Labeled for Base {
    type Tag = String;

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
    pub const LIMIT: u32 = limit_of(2);

    pub fn new(region: String) -> Self {
        Self { base: Base { label: String::new() }, region }
    }

    pub fn ping(&self) -> bool {
        !self.region.is_empty()
    }
}

/// A const fn used by an initializer.
pub const fn limit_of(n: u32) -> u32 {
    n
}

/// Reading modes.
pub enum Mode {
    Off,
    Level(u32),
    Custom { name: String },
}

/// Numeric codes.
pub enum Code {
    Ok = 0,
    Failed = 1,
}

/// Declares items in its body.
pub fn scaled(n: u32) -> u32 {
    struct Local {
        v: u32,
    }
    const FACTOR: u32 = limit_of(3);
    Local { v: n }.v * FACTOR
}
