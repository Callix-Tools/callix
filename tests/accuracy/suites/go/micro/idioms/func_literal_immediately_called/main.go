package main

func work() {}

func main() {
	func() {
		work()
	}()
}
