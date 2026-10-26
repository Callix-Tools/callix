fn g() {}

fn main() {
    let c = || g();
    c();
}
