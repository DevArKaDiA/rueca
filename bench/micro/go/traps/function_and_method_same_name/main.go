package main

func Close() int {
	return 0
}

type Conn struct{}

func (c *Conn) Close() int {
	return 1
}

func shutdown(c *Conn) int {
	return c.Close() + Close()
}

func main() {
	_ = shutdown(&Conn{})
}
