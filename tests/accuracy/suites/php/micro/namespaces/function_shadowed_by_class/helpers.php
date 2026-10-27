<?php
function once(callable $f) { return $f(); }
function logger($m) { return $m; }
