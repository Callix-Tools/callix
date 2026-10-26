<?php
namespace App;

class A {
    public function go() {}
    public function run() { $m = 'go'; $this->$m(); }
}
(new A())->run();
