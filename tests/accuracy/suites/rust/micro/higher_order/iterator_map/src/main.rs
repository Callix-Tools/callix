fn f(x: i32) -> i32 { x }

fn main() {
    let _: Vec<i32> = vec![1, 2].into_iter().map(f).collect();
}
