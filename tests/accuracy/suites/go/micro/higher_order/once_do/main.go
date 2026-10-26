package main

import "sync"

func setup() {}

func main() {
	var once sync.Once
	once.Do(setup)
}
