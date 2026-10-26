<?php
namespace App;

class Inv { public function __invoke() {} }
$o = new Inv();
$o();
