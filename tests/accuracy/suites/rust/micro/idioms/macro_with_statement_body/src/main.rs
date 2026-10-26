fn setup() {}
fn run(x: i32) {}

macro_rules! launch {
    ($x:expr) => {{
        setup();
        run($x);
    }};
}

fn main() {
    launch!(1);
}
