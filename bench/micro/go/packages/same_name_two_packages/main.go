package main

import (
	"example.com/same_name_two_packages/alpha"
	"example.com/same_name_two_packages/beta"
)

func runAlpha() int {
	return alpha.Run()
}

func runBeta() int {
	return beta.Run() + beta.Extra()
}

func main() {
	_ = runAlpha()
	_ = runBeta()
}
