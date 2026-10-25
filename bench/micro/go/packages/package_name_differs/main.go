package main

import "example.com/package_name_differs/lib/v2impl"

func run() int {
	return impl.Do(2)
}

func main() {
	_ = run()
}
