package main

import "sort"

func less(i, j int) bool { return i < j }

func main() {
	sort.Slice([]int{2, 1}, less)
}
