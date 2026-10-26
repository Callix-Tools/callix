<?php
namespace App;

class Facade {
    public static function __callStatic($name, $args) {}
}

Facade::anything();
