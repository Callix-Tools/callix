<?php
namespace App;

class A {
    public static function create(): static { return new static(); }
    public function run() {}
}
A::create()->run();
