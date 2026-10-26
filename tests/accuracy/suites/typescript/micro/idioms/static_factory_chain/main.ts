class A {
  static create(): A { return new A(); }
  run() {}
}
A.create().run();
