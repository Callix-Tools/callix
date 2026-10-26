<?php
namespace App;

abstract class A {
    abstract public function m();
    public function run() { $this->m(); }
}
class B extends A { public function m() {} }
(new B())->run();
