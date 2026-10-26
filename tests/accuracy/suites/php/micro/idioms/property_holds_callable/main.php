<?php
namespace App;

function cb() {}

class Worker {
    private $f;
    public function __construct(callable $f) { $this->f = $f; }
    public function run() { ($this->f)(); }
}

(new Worker('App\\cb'))->run();
