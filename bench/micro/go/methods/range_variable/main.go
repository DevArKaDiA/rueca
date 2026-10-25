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

func stopAll(servers []*Server) string {
	out := ""
	for _, s := range servers {
		out += s.Stop()
	}
	return out
}

func main() {
	_ = stopAll(nil)
}
