<?php
namespace App;

interface Root { public function info(); }
interface Contract extends Root {}
abstract class Base implements Contract {}
abstract class Child extends Base {
    public function run() { return $this->info(); }
}
class Impl extends Child { public function info() { return 1; } }
(new Impl())->run();
