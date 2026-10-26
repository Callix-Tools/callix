mod a {
    pub fn f() {}
}
use a::f as g;

fn main() { g(); }
