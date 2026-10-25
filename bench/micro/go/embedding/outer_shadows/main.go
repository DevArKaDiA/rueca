package main

type Base struct{}

func (Base) Hello() string {
	return "hello"
}

type Derived struct {
	Base
}

func (Derived) Hello() string {
	return "derived"
}

func viaOuter() string {
	d := Derived{}
	return d.Hello()
}

func viaField() string {
	d := Derived{}
	return d.Base.Hello()
}

func main() {
	_ = viaOuter()
	_ = viaField()
}
