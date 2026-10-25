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

func current() *Server {
	return &Server{name: "cur"}
}

func run() string {
	return current().Start()
}

func main() {
	_ = run()
}
