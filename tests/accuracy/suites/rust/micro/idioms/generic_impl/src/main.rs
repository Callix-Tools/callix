struct Foo<T>(T);
impl<T> Foo<T> { fn m(&self) {} }

fn main() { Foo(1).m(); }
