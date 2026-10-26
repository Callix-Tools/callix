mod a {
    mod hidden {
        pub fn f() {}
    }
    pub use self::hidden::f;
}

fn main() { a::f(); }
