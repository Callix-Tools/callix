<?php
namespace App;

trait T { public function m() {} }
class A { use T; }
(new A())->m();
