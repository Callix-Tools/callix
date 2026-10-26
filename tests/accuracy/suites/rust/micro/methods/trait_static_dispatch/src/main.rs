trait Tr { fn m(&self); }
struct T;
impl Tr for T { fn m(&self) {} }

fn main() { T.m(); }
