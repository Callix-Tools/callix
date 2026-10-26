<?php
namespace App;

function f($n) { return $n ? f($n - 1) : 0; }
f(3);
