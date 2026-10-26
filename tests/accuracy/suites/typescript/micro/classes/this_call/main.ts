class A {
  f() { this.g(); }
  g() {}
}
new A().f();
