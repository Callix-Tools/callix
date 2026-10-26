fn one() {}
fn two() {}

fn main() {
    let fs: [fn(); 2] = [one, two];
    for f in fs {
        f();
    }
}
