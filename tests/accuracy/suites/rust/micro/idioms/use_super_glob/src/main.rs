fn f() {}

mod inner {
    use super::*;
    pub fn g() { f(); }
}

fn main() { inner::g(); }
