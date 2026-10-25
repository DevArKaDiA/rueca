// Package server is the HTTP server.
package server

import (
	"example.com/shop/internal/store"
)

// Server serves requests.
type Server struct {
	Base
	store *store.Store
	name  string // trailing comment, not a doc
	// Hits counts handled requests.
	Hits int
}

// NewServer returns a server over st.
func NewServer(st *store.Store) *Server {
	return &Server{store: st, name: "shop"}
}

// Start starts serving.
func (s *Server) Start() error {
	v, ok := s.store.Get("boot")
	s.Hits++
	s.Log(v)
	_ = ok
	return nil
}
