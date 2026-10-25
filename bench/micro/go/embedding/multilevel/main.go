package main

type C struct{}

func (C) Deep() int {
	return 3
}

type B struct {
	C
}

func (B) Mid() int {
	return 2
}

type A struct {
	B
}

func run() int {
	a := A{}
	return a.Deep() + a.Mid()
}

func main() {
	_ = run()
}
