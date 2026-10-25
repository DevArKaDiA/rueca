package main

import "example.com/package_shadowed_by_variable/util"

type Tool struct{}

func (Tool) Reverse(s string) string {
	return s
}

func run() string {
	util := Tool{}
	return util.Reverse("x")
}

func pkg() string {
	return util.Reverse("y")
}

func main() {
	_ = run()
	_ = pkg()
}
