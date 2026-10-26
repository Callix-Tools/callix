struct T;
impl T {
    fn new() -> T { T }
    fn make() -> T { Self::new() }
}

fn main() { T::make(); }
