package main

func g() {}

type Handler func()

type S struct{ Handler }

func main() {
	s := S{Handler: g}
	s.Handler()
}
