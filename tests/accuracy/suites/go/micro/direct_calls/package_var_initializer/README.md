A package-level initializer's calls belong to the variable it initialises: `var x = helper()` is an edge `m.x -> m.helper`, and the body of `var fn = func(t T) {...}` is `m.fn`'s.
