package main

type Greeter interface {
	Greet() string
}

type English struct{}

func (English) Greet() string {
	return "hello"
}

type Spanish struct{}

func (Spanish) Greet() string {
	return "hola"
}

func run() string {
	e := English{}
	var g Greeter = Spanish{}
	return e.Greet() + g.Greet()
}

func main() {
	_ = run()
}
