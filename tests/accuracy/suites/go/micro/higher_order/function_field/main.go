package main

func g() {}

type S struct{ fn func() }

func main() {
	s := S{fn: g}
	s.fn()
}
