package main

type Base struct{}

func (Base) Hello() string {
	return "hello"
}

type Derived struct {
	Base
}

func run() string {
	d := Derived{}
	return d.Hello()
}

func main() {
	_ = run()
}
