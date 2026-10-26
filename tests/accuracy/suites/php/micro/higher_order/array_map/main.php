<?php
namespace App;

function f($x) { return $x; }
array_map('App\\f', [1, 2]);
