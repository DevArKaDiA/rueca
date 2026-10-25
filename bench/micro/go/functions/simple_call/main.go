package main

func helper() int {
	return 1
}

func run() int {
	return helper()
}

func main() {
	run()
}
