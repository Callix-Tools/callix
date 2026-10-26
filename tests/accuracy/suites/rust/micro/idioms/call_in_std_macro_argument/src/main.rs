fn compute() -> i32 { 1 }
fn check() -> bool { true }
fn label() -> String { String::new() }

fn main() {
    println!("{}", compute());
    assert!(check());
    let _ = format!("{}", label());
}
