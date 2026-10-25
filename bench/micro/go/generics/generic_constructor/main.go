package main

type Box[T any] struct {
	v T
}

func NewBox[T any](v T) *Box[T] {
	return &Box[T]{v: v}
}

func (b *Box[T]) Get() T {
	return b.v
}

func run() int {
	b := NewBox(5)
	return b.Get()
}

func main() {
	_ = run()
}
