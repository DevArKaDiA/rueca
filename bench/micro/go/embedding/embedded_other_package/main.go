package main

import "example.com/embedded_other_package/base"

type Handler struct {
	base.Core
}

func (h *Handler) Serve() string {
	return h.Describe()
}

func main() {
	h := &Handler{}
	_ = h.Serve()
}
