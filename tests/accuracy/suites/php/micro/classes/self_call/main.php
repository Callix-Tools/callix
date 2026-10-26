<?php
namespace App;

class A {
    public static function a() { self::b(); }
    public static function b() {}
}
A::a();
