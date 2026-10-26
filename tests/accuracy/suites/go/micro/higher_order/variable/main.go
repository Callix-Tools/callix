package main

func g() {}

func main() {
	f := g
	f()
}
