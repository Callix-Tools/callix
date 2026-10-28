<?php
namespace App;

class A {
    public function val() {}
    public static function make(): static { return new static(); }
    public function same(self $o) { $o->val(); }
    public function fresh() { (new static)->val(); }
    public function viaSelf() { self::make()->val(); }
}
class B extends A {
    public function val() { parent::val(); }
    public function up() { (new parent)->val(); }
}
