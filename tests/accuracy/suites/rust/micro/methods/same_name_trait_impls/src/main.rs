use std::ops::Add;

struct S(i32);

impl Add<S> for S {
    type Output = S;
    fn add(self, r: S) -> S {
        S(self.0 + r.0)
    }
}

impl<'a> Add<&'a S> for S {
    type Output = S;
    fn add(self, r: &'a S) -> S {
        S(self.0 + r.0)
    }
}

fn by_value(a: S, b: S) -> S {
    <S as Add<S>>::add(a, b)
}

fn by_ref(a: S, b: S) -> S {
    <S as Add<&S>>::add(a, &b)
}

fn main() {
    let _ = by_value(S(1), S(2));
    let _ = by_ref(S(1), S(2));
}
