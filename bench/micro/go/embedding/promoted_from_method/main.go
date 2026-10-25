package main

type Logger struct{}

func (l *Logger) Log(msg string) string {
	return msg
}

type Service struct {
	*Logger
	name string
}

func (s *Service) Run() string {
	return s.Log("run " + s.name)
}

func main() {
	s := &Service{Logger: &Logger{}}
	_ = s.Run()
}
