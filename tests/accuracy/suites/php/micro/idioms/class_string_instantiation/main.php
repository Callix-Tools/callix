<?php
namespace App;

class Made {
    public function __construct() {}
}

function build() {
    $class = Made::class;
    return new $class();
}
build();
