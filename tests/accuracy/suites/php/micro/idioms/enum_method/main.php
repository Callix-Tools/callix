<?php
namespace App;

enum E {
    case A;
    public function f() {}
}
E::A->f();
