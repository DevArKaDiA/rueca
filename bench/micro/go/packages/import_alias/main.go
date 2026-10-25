package main

import u "example.com/import_alias/util"

func run() string {
	return u.Upper(u.Reverse("abc"))
}

func main() {
	_ = run()
}
