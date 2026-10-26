package main

type T struct{}

func (T) Run() {}

func Run() {}

func main() {
	Run()
	T{}.Run()
}
