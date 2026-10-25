package main

func fact(n int) int {
	if n <= 1 {
		return 1
	}
	return n * fact(n-1)
}

func isEven(n int) bool {
	if n == 0 {
		return true
	}
	return isOdd(n - 1)
}

func isOdd(n int) bool {
	if n == 0 {
		return false
	}
	return isEven(n - 1)
}

func main() {
	_ = fact(5)
	_ = isEven(4)
}
