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

type App struct {
	srv *Server
}

func (a *App) Run() string {
	return a.srv.Start()
}

func main() {
	a := &App{srv: &Server{}}
	_ = a.Run()
}
