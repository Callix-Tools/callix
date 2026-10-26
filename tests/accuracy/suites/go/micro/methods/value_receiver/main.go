package main

type T struct{}

func (T) M() {}

func main() { T{}.M() }
