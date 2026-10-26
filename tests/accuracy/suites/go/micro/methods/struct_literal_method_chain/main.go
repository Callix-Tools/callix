package main

type B struct{}

func (b *B) Add() *B { return b }
func (b *B) Done() {}

func main() { (&B{}).Add().Add().Done() }
