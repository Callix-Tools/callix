<?php
namespace App;

function work() {}
function run(callable $f) { $f(); }

run(function () { work(); });
