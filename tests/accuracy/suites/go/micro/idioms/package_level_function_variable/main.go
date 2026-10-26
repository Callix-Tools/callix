package main

func g() {}

var handler = g

func main() { handler() }
