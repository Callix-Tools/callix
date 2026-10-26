<?php
namespace App;

class A { public function m() {} }
class B extends A { public function m() {} }
(new B())->m();
