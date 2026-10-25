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

func NewServer(name string) *Server {
	return &Server{name: name}
}

func run() string {
	srv := NewServer("a")
	return srv.Start()
}

func main() {
	_ = run()
}
