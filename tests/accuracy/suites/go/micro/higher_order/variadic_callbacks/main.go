package main

func one() {}
func two() {}

func run(fns ...func()) {
	for _, fn := range fns {
		fn()
	}
}

func main() {
	run(one, two)
}
