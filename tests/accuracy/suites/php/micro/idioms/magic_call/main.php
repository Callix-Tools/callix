<?php
namespace App;

class Proxy {
    public function __call($name, $args) {}
}

(new Proxy())->anything();
