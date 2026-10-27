<?php
namespace App\Log;

require __DIR__ . '/helpers.php';

use Lib\Once;

class Logger {}

function viaAlias() {
    return once(fn () => 1);
}

function viaNamespace() {
    return logger('x');
}
