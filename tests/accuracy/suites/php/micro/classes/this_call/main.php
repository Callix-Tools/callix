<?php
namespace App;

class A {
    public function f() { $this->g(); }
    public function g() {}
}
(new A())->f();
