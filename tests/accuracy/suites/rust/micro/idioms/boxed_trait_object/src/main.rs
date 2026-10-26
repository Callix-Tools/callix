trait Tr { fn m(&self); }
struct T;
impl Tr for T { fn m(&self) {} }

fn main() {
    let b: Box<dyn Tr> = Box::new(T);
    b.m();
}
