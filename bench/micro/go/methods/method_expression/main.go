package main

type Counter struct {
	n int
}

func (c Counter) Value() int {
	return c.n
}

func (c *Counter) Inc() {
	c.n++
}

func run() int {
	c := Counter{}
	(*Counter).Inc(&c)
	return Counter.Value(c)
}

func main() {
	_ = run()
}
