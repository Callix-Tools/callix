interface I { m(): void }
class A implements I { m() {} }
function use(i: I) { i.m(); }
use(new A());
