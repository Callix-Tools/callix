package main

type Inner struct{}

func (Inner) M() {}

type Outer struct{ Inner }

func main() { Outer{}.M() }
