class A {
  async f() { await this.g(); }
  async g() {}
}
new A().f();
