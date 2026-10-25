package main

import "sync"

func work(wg *sync.WaitGroup) {
	defer wg.Done()
}

func cleanup() {}

func run() {
	var wg sync.WaitGroup
	wg.Add(1)
	go work(&wg)
	defer cleanup()
	wg.Wait()
}

func main() {
	run()
}
