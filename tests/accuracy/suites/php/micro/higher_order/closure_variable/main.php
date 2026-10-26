<?php
namespace App;

function g() {}
$f = function () { g(); };
$f();
