<?php
namespace App;

function cb() {}

function main() {
    $f = \Closure::fromCallable('App\\cb');
    $f();
}
main();
