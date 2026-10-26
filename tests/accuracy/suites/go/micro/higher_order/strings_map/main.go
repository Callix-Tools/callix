package main

import "strings"

func upper(r rune) rune { return r }

func main() {
	_ = strings.Map(upper, "abc")
}
