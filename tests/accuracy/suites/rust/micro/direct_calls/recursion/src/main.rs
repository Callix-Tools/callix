fn f(n: u32) -> u32 { if n == 0 { 0 } else { f(n - 1) } }

fn main() { f(3); }
