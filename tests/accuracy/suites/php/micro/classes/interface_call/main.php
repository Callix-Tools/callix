<?php
namespace App;

interface I { public function m(); }
class A implements I { public function m() {} }
function use_it(I $i) { $i->m(); }
use_it(new A());
