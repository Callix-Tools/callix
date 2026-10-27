<?php
namespace App;

trait T { public function info() { return 2; } }
class Base { public function info() { return 1; } }
class Child extends Base {
    use T;
    public function run() { return $this->info(); }
}
(new Child())->run();
