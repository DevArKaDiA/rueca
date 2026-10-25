package main

type List[T any] struct {
	items []T
}

func (l *List[T]) Push(v T) {
	l.grow()
	l.items = append(l.items, v)
}

func (l *List[T]) grow() {
	if cap(l.items) == 0 {
		l.items = make([]T, 0, 4)
	}
}

func (l *List[T]) Len() int {
	return len(l.items)
}

func run() int {
	l := &List[int]{}
	l.Push(1)
	return l.Len()
}

func main() {
	_ = run()
}
