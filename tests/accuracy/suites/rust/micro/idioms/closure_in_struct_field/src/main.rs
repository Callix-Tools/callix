struct S { f: fn() }
fn g() {}

fn main() {
    let s = S { f: g };
    (s.f)();
}
