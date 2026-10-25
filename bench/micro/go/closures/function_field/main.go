package main

type Handler struct {
	fn func() string
}

func (h Handler) Call() string {
	return h.fn()
}

func ping() string {
	return "pong"
}

func main() {
	h := Handler{fn: ping}
	_ = h.Call()
}
