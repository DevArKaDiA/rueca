package main

type Cat struct{}

func (Cat) Name() string {
	return "cat"
}

type Dog struct{}

func (Dog) Name() string {
	return "dog"
}

func catName() string {
	c := Cat{}
	return c.Name()
}

func dogName(d Dog) string {
	return d.Name()
}

func main() {
	_ = catName()
	_ = dogName(Dog{})
}
