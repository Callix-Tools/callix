class A { m() {} }
class B extends A { m() { super.m(); } }
new B().m();
