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

func run(g Greeter) string {
	return g.(Spanish).Greet()
}

func main() {
	_ = run(Spanish{})
}
