<?php
namespace App;

class A {
    public function m() {}
    public function run() { array_map([$this, 'm'], [1]); }
}
(new A())->run();
