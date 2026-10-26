trait Tr { fn m(&self); }
struct T;
impl Tr for T { fn m(&self) {} }

fn use_it<X: Tr>(x: X) { x.m(); }

fn main() { use_it(T); }
