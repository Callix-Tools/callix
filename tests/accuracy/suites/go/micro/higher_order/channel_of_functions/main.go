package main

func job() {}

func main() {
	ch := make(chan func(), 1)
	ch <- job
	f := <-ch
	f()
}
