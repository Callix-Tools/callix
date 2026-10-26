<?php
namespace App;

function cb() {}
function run(callable $f) { $f(); }
run('App\\cb');
