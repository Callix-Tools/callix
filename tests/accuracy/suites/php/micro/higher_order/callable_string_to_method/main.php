<?php
namespace App;

function cb() {}

class Runner {
    public function run(callable $f) { $f(); }
}

(new Runner())->run('App\\cb');
