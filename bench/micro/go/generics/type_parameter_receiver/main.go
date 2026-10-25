package main

import "fmt"

type ID int

func (i ID) String() string {
	return fmt.Sprintf("id-%d", int(i))
}

func Stringify[T fmt.Stringer](x T) string {
	return x.String()
}

func run() string {
	return Stringify(ID(7))
}

func main() {
	_ = run()
}
