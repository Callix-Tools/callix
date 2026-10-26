<?php
namespace App;

function one() {}
function two() {}

function run(callable $first, callable $second) { $second(); }

run(second: 'App\\two', first: 'App\\one');
