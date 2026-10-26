package main

type Box[T any] struct{}

func (b Box[T]) Get() {}

func main() { Box[int]{}.Get() }
