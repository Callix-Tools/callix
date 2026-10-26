fn f() {}

macro_rules! call_it {
    ($g:ident) => { $g() };
}

fn main() {
    call_it!(f);
}
