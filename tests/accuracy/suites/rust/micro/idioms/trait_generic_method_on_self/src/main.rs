trait Tr {
    fn a(&self) { self.b(); }
    fn b(&self) {}
}
struct T;
impl Tr for T {}

fn main() { T.a(); }
