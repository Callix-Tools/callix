package main

type Inner struct{}

func (Inner) M() {}

type Outer struct{ Inner }

func (Outer) M() {}

func main() { Outer{}.M() }
