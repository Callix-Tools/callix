<?php
namespace App;

trait T {
    abstract public function m();
    public function run() { $this->m(); }
}
class A { use T; public function m() {} }
(new A())->run();
