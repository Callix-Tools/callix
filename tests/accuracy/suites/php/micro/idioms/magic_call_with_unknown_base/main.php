<?php
namespace App;

use Vendor\Base;

class Proxy extends Base {
    public function __call($name, $args) {}
}

(new Proxy())->inherited();
