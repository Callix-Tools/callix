<?php
namespace App;

function g() {}
function f() {
    $cb = function () use (&$x) { g(); };
    $cb();
}
