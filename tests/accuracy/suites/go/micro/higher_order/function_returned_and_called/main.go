package main

func target() {}

func pick() func() { return target }

func main() {
	pick()()
}
