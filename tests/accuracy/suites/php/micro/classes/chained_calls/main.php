<?php
namespace App;

class A {
    public function a(): static { return $this; }
    public function b() {}
}
(new A())->a()->b();
