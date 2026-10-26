package main

func g() {}
func build() func() { return g }

func main() { build()() }
