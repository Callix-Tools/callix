<?php
namespace App;

require __DIR__ . '/Lib.php';
require __DIR__ . '/globals.php';

use function A\f as g;

function k() {}

function run() {
    g();
    k();
}
