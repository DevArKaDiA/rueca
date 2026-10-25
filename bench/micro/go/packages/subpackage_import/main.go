package main

import "example.com/subpackage_import/util"

func run() string {
	return util.Reverse("abc")
}

func main() {
	_ = run()
}
