<?php
namespace App;

function leaf() {}
function inner(callable $f) { $f(); }
function outer(callable $g) { inner($g); }

outer('App\\leaf');
