package main

import (
	"fmt"

	"example.com/fixture/store"
)

// DefaultRegion is a package-level variable.
var DefaultRegion = "eu"

// Build returns an engine for the region.
func Build(region string) store.Engine {
	engine := store.Engine{Region: region}
	engine.Ping()
	return engine
}

// Inspect takes an interface value and calls through it.
func Inspect(item store.Labeled) string {
	return item.Describe()
}

func main() {
	fmt.Println(Inspect(Build(DefaultRegion)))
}
