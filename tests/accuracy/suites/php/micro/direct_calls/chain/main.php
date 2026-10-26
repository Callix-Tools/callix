<?php
namespace App;

function a() { b(); }
function b() { c(); }
function c() {}
a();
