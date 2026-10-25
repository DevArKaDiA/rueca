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

func say(g Greeter) string {
	return g.Greet()
}

func main() {
	_ = say(English{})
	_ = say(Spanish{})
}
