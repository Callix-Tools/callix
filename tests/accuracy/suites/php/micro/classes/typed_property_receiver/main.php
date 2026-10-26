<?php
namespace App;

class B { public function m() {} }
class A {
    private B $b;
    public function __construct() { $this->b = new B(); }
    public function f() { $this->b->m(); }
}
(new A())->f();
