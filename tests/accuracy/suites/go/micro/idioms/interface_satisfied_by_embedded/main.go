package main

type Runner interface{ Run() }

type Base struct{}

func (Base) Run() {}

type Job struct{ Base }

func start(r Runner) { r.Run() }

func main() {
	start(Job{})
}
