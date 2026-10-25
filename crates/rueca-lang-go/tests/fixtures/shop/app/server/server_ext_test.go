package server_test

import (
	"testing"

	"example.com/shop/server"
)

func ExampleServer() {
	srv := server.NewServer(nil)
	srv.Start()
}

func TestExternal(t *testing.T) {
	var s server.Server
	s.Handle(nil)
}
