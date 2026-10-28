macro_rules! twice {
    ($e:expr) => { $e + $e };
}

fn main() {
    let _ = twice!(1);
}
