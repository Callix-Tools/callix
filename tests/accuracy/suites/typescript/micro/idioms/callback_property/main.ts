function g() {}
class A {
  constructor(private cb: () => void) {}
  run() { this.cb(); }
}
new A(g).run();
