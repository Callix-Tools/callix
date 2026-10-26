fn id<T>(x: T) -> T { x }

fn main() { id(1); id::<&str>("a"); }
