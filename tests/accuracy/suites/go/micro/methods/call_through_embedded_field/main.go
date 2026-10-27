package main

type Group struct{}

func (g *Group) Use() {}

type Engine struct{ Group }

func main() {
	e := &Engine{}
	e.Group.Use()
}
