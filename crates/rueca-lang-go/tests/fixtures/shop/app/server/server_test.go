package server

import "testing"

func TestStart(t *testing.T) {
	s := NewServer(nil)
	if err := s.Start(); err != nil {
		t.Fatal(err)
	}
	newFixture(t)
}

func BenchmarkStart(b *testing.B) {}

func newFixture(t *testing.T) *Server { return nil }

func Testlowercase(t *testing.T) {}
