package main

type Handler struct {
	OnDone func()
}

func finish() {}

func fire(h Handler) { h.OnDone() }

func main() {
	fire(Handler{OnDone: finish})
}
