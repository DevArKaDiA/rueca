package main

func helper() int {
	return 1
}

func run() int {
	f := func() int {
		return helper()
	}
	return f()
}

func main() {
	_ = run()
}
