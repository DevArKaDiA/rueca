package main

type Base struct{}

func (Base) Hello() string {
	return "hello"
}

type Derived struct {
	*Base
}

func run() string {
	d := &Derived{Base: &Base{}}
	return d.Hello()
}

func main() {
	_ = run()
}
