package main

import "example.com/type_from_other_package/stack"

func run() int {
	s := stack.New()
	s.Push(1)
	s.Push(2)
	return s.Pop()
}

func main() {
	_ = run()
}
