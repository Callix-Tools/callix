abstract class A {
  abstract m(): void;
  run() { this.m(); }
}
class B extends A { m() {} }
new B().run();
