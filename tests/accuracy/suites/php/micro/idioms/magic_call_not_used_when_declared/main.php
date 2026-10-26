<?php
namespace App;

class Proxy {
    public function real() {}
    public function __call($name, $args) {}
}

(new Proxy())->real();
