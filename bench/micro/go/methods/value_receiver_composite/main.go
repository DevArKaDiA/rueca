package main

type Counter struct {
	n int
}

func (c Counter) Value() int {
	return c.n
}

func run() int {
	c := Counter{n: 3}
	return c.Value()
}

func main() {
	_ = run()
}
