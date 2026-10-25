package main

func helper() int {
	return 1
}

func run() int {
	return func() int {
		return helper() + 1
	}()
}

func main() {
	_ = run()
}
