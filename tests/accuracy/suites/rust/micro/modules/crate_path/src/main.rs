mod a {
    pub fn f() { crate::g(); }
}
fn g() {}

fn main() { a::f(); }
