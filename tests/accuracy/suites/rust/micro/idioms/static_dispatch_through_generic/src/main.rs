trait Greeter { fn hi(&self); }

struct A;
impl Greeter for A { fn hi(&self) {} }

fn run<T: Greeter>(t: T) { t.hi(); }

fn main() { run(A); }
