package main

import "strconv"

func Map[T, U any](xs []T, f func(T) U) []U {
	out := make([]U, 0, len(xs))
	for _, x := range xs {
		out = append(out, f(x))
	}
	return out
}

func inferred() []string {
	return Map([]int{1, 2}, strconv.Itoa)
}

func explicit() []string {
	return Map[int, string]([]int{3}, strconv.Itoa)
}

func main() {
	_ = inferred()
	_ = explicit()
}
