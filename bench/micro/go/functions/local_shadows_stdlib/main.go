package main

import "fmt"

// Println is a project function with the same name as fmt.Println.
func Println(s string) string {
	return "[" + s + "]"
}

func logStd() {
	fmt.Println("std")
}

func logLocal() string {
	return Println("local")
}

func main() {
	logStd()
	_ = logLocal()
}
