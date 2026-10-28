<?php
namespace App;

function cb() {}
function run(callable $f, bool $c) {
    if ($c) {
        $f = 'App\\other';
    }
    $f();
}
function other() {}

run('App\\cb', false);
run('App\\cb', true);
