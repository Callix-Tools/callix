fn a() {}
fn b() {}

macro_rules! call_all {
    ($($f:ident),*) => { $( $f(); )* };
}

fn main() {
    call_all!(a, b);
}
