<?php
namespace App;

function cb() {}
function run(callable $f) { call_user_func($f); }

run('App\\cb');
