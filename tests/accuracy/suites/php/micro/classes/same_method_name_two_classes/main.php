<?php
namespace App;

class A { public function run() {} }
class B { public function run() {} }
function f(A $a, B $b) { $a->run(); $b->run(); }
f(new A(), new B());
