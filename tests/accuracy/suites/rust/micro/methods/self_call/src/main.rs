struct T;
impl T {
    fn a(&self) { self.b(); }
    fn b(&self) {}
}

fn main() { T.a(); }
