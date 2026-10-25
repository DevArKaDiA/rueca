package main

var table = compute()

func compute() []int {
	return []int{1, 2, 3}
}

func use() int {
	return len(table)
}

func run() int {
	return use()
}

func main() {
	_ = run()
}
