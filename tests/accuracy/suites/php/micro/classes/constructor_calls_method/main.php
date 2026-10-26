<?php
namespace App;

class A {
    public function __construct() { $this->init(); }
    private function init() {}
}
new A();
