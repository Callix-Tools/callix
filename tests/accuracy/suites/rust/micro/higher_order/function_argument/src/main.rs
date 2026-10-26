fn g() {}
fn run(f: fn()) { f(); }

fn main() { run(g); }
