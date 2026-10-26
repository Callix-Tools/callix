package main

func g() {}

func main() {
	func() { g() }()
}
