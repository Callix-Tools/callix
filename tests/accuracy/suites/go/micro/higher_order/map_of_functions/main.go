package main

func a() {}
func b() {}

func main() {
	m := map[string]func(){"a": a, "b": b}
	m["a"]()
}
