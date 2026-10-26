package main

type T struct{}

func (T) M() {}

func main() {
	f := T{}.M
	f()
}
