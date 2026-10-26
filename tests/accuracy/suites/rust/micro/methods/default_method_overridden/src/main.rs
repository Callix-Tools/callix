trait Tr { fn a(&self) {} }
struct T;
impl Tr for T { fn a(&self) {} }

fn main() { T.a(); }
