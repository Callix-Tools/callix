package main

type HandlerFunc func()

func (h HandlerFunc) Serve() { h() }

func g() {}

func main() { HandlerFunc(g).Serve() }
