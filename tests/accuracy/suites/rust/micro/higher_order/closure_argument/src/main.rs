fn g() {}
fn run<F: Fn()>(f: F) { f(); }

fn main() { run(|| g()); }
