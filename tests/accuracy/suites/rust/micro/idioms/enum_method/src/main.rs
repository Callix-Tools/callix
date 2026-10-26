enum E { A }
impl E { fn m(&self) {} }

fn main() { E::A.m(); }
