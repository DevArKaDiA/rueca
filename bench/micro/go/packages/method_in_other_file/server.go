package web

type Server struct {
	addr string
}

func (s *Server) Start() string {
	return s.listen()
}
