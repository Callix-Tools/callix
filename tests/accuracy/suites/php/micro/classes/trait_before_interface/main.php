<?php
namespace App;

interface Contract { public function info(); }
trait T { public function info() { return 1; } }
abstract class Base { use T; }
class Child extends Base implements Contract {
    public function run() { return $this->info(); }
}
(new Child())->run();
