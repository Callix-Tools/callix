<?php
namespace App;

interface Contract { public function info(); }
abstract class Base { public function info() { return 1; } }
class Child extends Base implements Contract {
    public function run() { return $this->info(); }
}
(new Child())->run();
