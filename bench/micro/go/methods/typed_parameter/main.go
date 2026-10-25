package main

type Server struct {
	name string
}

func (s *Server) Start() string {
	return "start " + s.name
}

func (s *Server) Stop() string {
	return "stop " + s.name
}

func handle(s *Server) string {
	return s.Start() + s.Stop()
}

func main() {
	_ = handle(&Server{})
}
