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
	servers := []*Server{{name: "a"}}
	byName := map[string]*Server{"b": {name: "b"}}
	return servers[0].Start() + byName["b"].Stop()
}

func main() {
	_ = run()
}
