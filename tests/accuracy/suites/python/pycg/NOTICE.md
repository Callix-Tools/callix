# PyCG micro-benchmark

Vendored, unmodified, from the artifact of *PyCG: Practical Call Graph Generation
in Python* (Salis et al., ICSE 2021):
<https://github.com/vitsalis/pycg-evaluation>, commit
`ce37eb5668465b0c17371914e863d699826447ee`, directory `data/micro-benchmark`.

112 minimal programs in 16 categories of Python language features. Each case has
a hand-written `callgraph.json` (`{caller: [callee]}`, names relative to the case
directory, `main` being the module-level code of `main.py`) which is the ground
truth. The artifact is licensed under the Apache License 2.0 (`LICENSE`).
