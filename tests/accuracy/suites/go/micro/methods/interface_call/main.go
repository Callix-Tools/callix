package main

type I interface{ M() }

type T struct{}

func (T) M() {}

func use(i I) { i.M() }

func main() { use(T{}) }
