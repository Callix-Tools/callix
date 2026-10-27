package m

func helper() int { return 1 }

type T struct{}

func (T) Do() {}

var x = helper()

var fn = func(t T) {
	t.Do()
}

func Plain() int { return helper() }

func main() { Plain() }
