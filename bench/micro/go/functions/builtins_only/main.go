package main

import (
	"fmt"
	"strings"
)

func describe(items []string) string {
	items = append(items, "end")
	n := len(items)
	out := make([]string, 0, n)
	for _, it := range items {
		out = append(out, strings.ToUpper(it))
	}
	return fmt.Sprintf("%d: %s", n, strings.Join(out, ","))
}

func main() {
	fmt.Println(describe(nil))
}
