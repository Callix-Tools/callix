package main

func v() int { return 1 }
func f(x int) int { return x }

func main() { f(v()) }
