<?php
namespace App;

class Handler {
    public function handle() {}
}

function dispatch(callable $f) { $f(); }

dispatch([new Handler(), 'handle']);
