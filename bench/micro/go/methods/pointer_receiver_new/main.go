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

func run() string {
	s := &Server{name: "a"}
	return s.Start()
}

func main() {
	_ = run()
}
