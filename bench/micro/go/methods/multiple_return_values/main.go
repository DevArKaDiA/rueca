package main

import "errors"

type File struct {
	open bool
}

func (f *File) Close() error {
	f.open = false
	return nil
}

func Open(name string) (*File, error) {
	if name == "" {
		return nil, errors.New("empty")
	}
	return &File{open: true}, nil
}

func run() error {
	f, err := Open("x")
	if err != nil {
		return err
	}
	return f.Close()
}

func main() {
	_ = run()
}
