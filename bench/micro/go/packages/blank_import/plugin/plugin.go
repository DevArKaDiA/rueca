package plugin

var registry []string

func register(name string) {
	registry = append(registry, name)
}

func init() {
	register("plugin")
}
