package main

func f(n int) int {
	if n == 0 {
		return 0
	}
	return f(n - 1)
}

func main() { f(3) }
