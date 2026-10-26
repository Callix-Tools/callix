mod a {
    pub mod b {
        pub fn f() {}
    }
}

fn main() { a::b::f(); }
