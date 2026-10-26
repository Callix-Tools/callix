fn show(x: i32) {}

fn apply<F: Fn(i32)>(f: F, x: i32) { f(x) }

fn main() {
    apply(show, 1);
}
