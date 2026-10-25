package main

func compute() int {
	return 42
}

func run() int {
	compute := func() int { return 0 }
	return compute()
}

func other() int {
	return compute()
}

func main() {
	_ = run()
	_ = other()
}
