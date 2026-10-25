package main

func apply(f func(int) int, x int) int {
	return f(x)
}

func double(x int) int {
	return x * 2
}

func run() int {
	return apply(double, 2)
}

func main() {
	_ = run()
}
