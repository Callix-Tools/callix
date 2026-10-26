package main

type W interface{ Write() }

type S struct{ w W }

func (s S) f() { s.w.Write() }

func main() {}
