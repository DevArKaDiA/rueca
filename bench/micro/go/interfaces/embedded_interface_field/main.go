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

type Bot struct {
	Greeter
}

func (b Bot) Hello() string {
	return b.Greet()
}

func main() {
	b := Bot{Greeter: English{}}
	_ = b.Hello()
}
