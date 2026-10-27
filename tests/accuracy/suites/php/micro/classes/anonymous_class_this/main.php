<?php
namespace App;

class Base {
    public $items = [];
    public function run() {}
    public function passes() {}
}

class Host {
    public $items = 1;
    public function make() {
        $o = new class extends Base {
            public function passes() { return parent::passes(); }
            public function go() { $this->run(); $this->passes(); return $this->items; }
        };
        $o->go();
        $this->passes();
    }
    public function passes() {}
}
