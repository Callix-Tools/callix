package main

func show(x int) {}

func Apply[T any](x T, f func(T)) { f(x) }

func main() {
	Apply(1, show)
}
