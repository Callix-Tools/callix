mod a {
    pub struct T;
    impl T { pub fn m(&self) {} }
}

fn main() { a::T.m(); }
