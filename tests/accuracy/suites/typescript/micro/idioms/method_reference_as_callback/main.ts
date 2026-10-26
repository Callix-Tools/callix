class A {
  m() {}
}
function run(f: () => void) { f(); }
run(new A().m);
