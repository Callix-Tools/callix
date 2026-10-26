package main

type I interface{ M() }
type J interface{ I }

func use(j J) { j.M() }

func main() {}
