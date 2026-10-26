<?php
namespace App;

class Maths {
    public static function double($x) { return $x * 2; }
    public function all() { return array_map('App\\Maths::double', [1]); }
}
(new Maths())->all();
