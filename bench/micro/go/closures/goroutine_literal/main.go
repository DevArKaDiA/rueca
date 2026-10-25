package main

func produce(ch chan<- int) {
	ch <- 1
}

func consume(ch <-chan int) int {
	return <-ch
}

func run() int {
	ch := make(chan int, 1)
	go func() {
		produce(ch)
	}()
	return consume(ch)
}

func main() {
	_ = run()
}
