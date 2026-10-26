package main

type A struct{}

func (*A) M() {}

type B struct{ *A }

func main() { b := B{&A{}}; b.M() }
