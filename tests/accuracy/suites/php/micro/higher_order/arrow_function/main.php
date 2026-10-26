<?php
namespace App;

function g() { return 1; }
$f = fn() => g();
$f();
