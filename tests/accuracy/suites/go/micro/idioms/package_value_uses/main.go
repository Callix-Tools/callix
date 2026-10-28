package main

const limit = 3

var counter int

func helper() int { return limit }

func double(v int) int { return v * 2 }

var fn = helper

func main() {
	counter = limit + counter
	f := helper
	_ = f
	_ = double(counter)
	fn()
}
