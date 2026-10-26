<?php
namespace App;

class Util {
    public static function go() {}
}

function run(callable $f) { $f(); }

run('App\\Util::go');
