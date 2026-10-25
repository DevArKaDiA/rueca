package main

import _ "example.com/blank_import/plugin"

func run() int {
	return 1
}

func main() {
	_ = run()
}
