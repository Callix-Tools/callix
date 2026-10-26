<?php
namespace App;

class A { public function m() {} }
class B extends A {}
(new B())->m();
