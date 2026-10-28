const LIMIT: u32 = 3;
struct Cfg { n: u32 }
enum Mode { A, B }

fn limit() -> u32 { LIMIT }
fn build(n: u32) -> Cfg { Cfg { n } }
fn pick(m: Mode) -> u32 { match m { Mode::A => limit(), Mode::B => 0 } }

fn main() {
    let c = build(limit());
    let _ = pick(Mode::B);
    let Cfg { n } = c;
    let _ = n;
}
