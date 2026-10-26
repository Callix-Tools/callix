fn g() {}

fn outer() {
    fn inner() { g(); }
    inner();
}

fn main() { outer(); }
