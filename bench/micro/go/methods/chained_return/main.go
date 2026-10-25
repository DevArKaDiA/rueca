package main

type Config struct {
	port int
}

func (c *Config) Port() int {
	return c.port
}

type App struct {
	cfg *Config
}

func (a *App) Config() *Config {
	return a.cfg
}

func run(a *App) int {
	return a.Config().Port()
}

func main() {
	_ = run(&App{cfg: &Config{port: 80}})
}
