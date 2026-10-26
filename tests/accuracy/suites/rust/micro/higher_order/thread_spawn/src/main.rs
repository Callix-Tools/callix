use std::thread;

fn worker() {}

fn main() {
    let h = thread::spawn(worker);
    h.join().unwrap();
}
