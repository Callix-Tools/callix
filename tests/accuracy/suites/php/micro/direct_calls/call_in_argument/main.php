<?php
namespace App;

function v() { return 1; }
function f($x) { return $x; }
f(v());
