fn g() {}

mod a {
    pub fn f() { super::g(); }
}

fn main() { a::f(); }
