package web

func (s *Server) listen() string {
	return s.addr
}

func Boot() string {
	s := &Server{addr: ":80"}
	return s.Start()
}
