package main

type Reader struct{}

func (r *Reader) read() int {
	return 1
}

type Writer struct{}

func (w *Writer) read() int {
	return 2
}

func (w *Writer) Flush() int {
	return w.read()
}

func main() {
	w := &Writer{}
	_ = w.Flush()
}
