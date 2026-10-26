package main

type S struct{}

func (S) close() {}

func main() {
	s := S{}
	defer s.close()
}
