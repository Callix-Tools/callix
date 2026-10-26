package main

func sum(xs ...int) int { return len(xs) }

func main() { sum(1, 2, 3) }
