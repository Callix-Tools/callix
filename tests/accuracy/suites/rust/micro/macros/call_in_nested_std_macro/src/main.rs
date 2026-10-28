fn helper(x: i32) -> i32 {
    x + 1
}

fn main() {
    let w = vec![helper(0)];
    assert_eq!(w, vec!(helper(1)));
}
