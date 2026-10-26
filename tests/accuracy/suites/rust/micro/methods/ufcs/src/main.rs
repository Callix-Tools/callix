struct T;
impl T { fn m(&self) {} }

fn main() { T::m(&T); }
