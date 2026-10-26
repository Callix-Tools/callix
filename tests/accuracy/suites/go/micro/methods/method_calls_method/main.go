package main

type T struct{}

func (t T) A() { t.B() }
func (t T) B() {}

func main() { T{}.A() }
