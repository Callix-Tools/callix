trait Shape {
    fn area(&self) -> i32;
    fn double(&self) -> i32 { self.area() * 2 }
}

struct Sq;

impl Shape for Sq {
    fn area(&self) -> i32 { 1 }
}

fn main() {
    let s = Sq;
    s.double();
}
