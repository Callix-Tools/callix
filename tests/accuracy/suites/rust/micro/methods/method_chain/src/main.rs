struct B;
impl B {
    fn add(self) -> B { self }
    fn done(self) {}
}

fn main() { B.add().add().done(); }
