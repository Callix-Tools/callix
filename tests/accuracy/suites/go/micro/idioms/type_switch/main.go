package main

type T struct{}

func (T) M() {}

func f(x any) {
	switch v := x.(type) {
	case T:
		v.M()
	}
}

func main() { f(T{}) }
