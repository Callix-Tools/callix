<?php
namespace App;

function cb() {}
function run(callable $f) {
    $f = 'App\\other';
    $f();
}
function other() {}

run('App\\cb');
