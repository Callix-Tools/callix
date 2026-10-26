package main

func pair() (int, int) { return 1, 2 }

func main() { a, b := pair(); _, _ = a, b }
