package main

var ready bool

func setup() {
	ready = true
}

func run() bool {
	return ready
}

func init() {
	setup()
}

func main() {
	_ = run()
}
