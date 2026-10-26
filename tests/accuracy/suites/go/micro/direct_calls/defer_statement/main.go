package main

func g() {}

func main() { defer g() }
