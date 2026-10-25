package main

func f(x int) int { return x + 1 }

func g(x int) int { return x * 2 }

func h(x int) int { return x - 3 }

func run() int {
	return f(g(h(1)))
}

func main() {
	_ = run()
}
