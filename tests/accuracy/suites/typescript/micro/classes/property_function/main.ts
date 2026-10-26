function g() {}
class A {
  f = () => { g(); };
}
new A().f();
