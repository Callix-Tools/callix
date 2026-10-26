fn g() -> Result<(), ()> { Ok(()) }
fn f() -> Result<(), ()> { g()?; Ok(()) }

fn main() { let _ = f(); }
