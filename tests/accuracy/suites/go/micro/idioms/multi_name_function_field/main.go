package main

func f() {}

func g() {}

type S struct{ pre, post func() }

func main() {
	s := S{pre: f, post: g}
	s.post()
}
