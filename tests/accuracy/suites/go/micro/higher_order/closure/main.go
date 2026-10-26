package main

func g() {}
func build() func() { return func() { g() } }

func main() {
	f := build()
	f()
}
