package main

func one() {}
func two() {}

func main() {
	fns := []func(){one, two}
	for _, fn := range fns {
		fn()
	}
}
