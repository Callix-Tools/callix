<?php
namespace App;

class A {
    public static function make() { return new static(); }
    public function __construct() {}
}
A::make();
