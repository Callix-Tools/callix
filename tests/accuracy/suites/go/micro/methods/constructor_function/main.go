package main

type T struct{}

func New() *T { return &T{} }
func (t *T) Run() {}

func main() { New().Run() }
