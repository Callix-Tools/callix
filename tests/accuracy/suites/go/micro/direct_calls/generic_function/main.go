package main

func id[T any](x T) T { return x }

func main() { id(1); id[string]("a") }
