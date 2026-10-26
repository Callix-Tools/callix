trait Tr { fn make() -> Self; }
struct T;
impl Tr for T { fn make() -> T { T } }

fn main() { let _ = <T as Tr>::make(); }
