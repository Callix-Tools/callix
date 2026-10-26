<?php
namespace App;

function g() {}
function run(callable $f) { $f(); }

run(g(...));
