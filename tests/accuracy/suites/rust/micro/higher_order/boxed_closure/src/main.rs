fn g() {}

fn main() {
    let b: Box<dyn Fn()> = Box::new(|| g());
    b();
}
