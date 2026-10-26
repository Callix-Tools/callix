package main

type T struct{ next *T }

func (t *T) walk() {
	if t.next != nil {
		t.next.walk()
	}
}

func main() { (&T{}).walk() }
