package main

type S struct{}

func (S) run() {}

func main() {
	s := S{}
	go s.run()
}
