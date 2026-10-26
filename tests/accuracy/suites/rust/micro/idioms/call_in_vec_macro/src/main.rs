fn make() -> i32 { 1 }
fn size() -> usize { 2 }

fn main() {
    let _ = vec![make(); size()];
}
