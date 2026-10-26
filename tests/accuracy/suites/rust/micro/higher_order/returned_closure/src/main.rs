fn target() {}

fn make() -> impl Fn() {
    || target()
}

fn main() {
    let f = make();
    f();
}
