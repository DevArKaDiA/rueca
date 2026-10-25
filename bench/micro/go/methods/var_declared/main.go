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
	var s Server
	return s.Stop()
}

func main() {
	_ = run()
}
