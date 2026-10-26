package main

type T struct{ n int }

func (t *T) Inc() { t.n++ }

func main() {
	t := &T{}
	t.Inc()
}
