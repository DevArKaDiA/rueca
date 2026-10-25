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
	switch v := g.(type) {
	case English:
		return v.Greet()
	default:
		return ""
	}
}

func main() {
	_ = run(English{})
}
