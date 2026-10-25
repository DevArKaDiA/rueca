package main

import . "example.com/dot_import/util"

func run() string {
	return Reverse("abc")
}

func main() {
	_ = run()
}
