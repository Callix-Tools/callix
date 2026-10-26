package main

func cb() {}
func run(f func()) { f() }

func main() { run(cb) }
