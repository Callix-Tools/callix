fn key(x: &i32) -> i32 { *x }

fn main() {
    let mut v = vec![2, 1];
    v.sort_by_key(key);
}
