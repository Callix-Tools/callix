fn g() {}

fn main() {
    let f: fn() = g;
    f();
}
