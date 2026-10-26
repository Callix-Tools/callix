<?php
namespace App;

class A { public function m() {} }
function f(?A $a) { $a?->m(); }
