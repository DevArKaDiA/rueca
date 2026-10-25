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
	var g Greeter = English{}
	return g.Greet()
}

func main() {
	_ = run()
}
