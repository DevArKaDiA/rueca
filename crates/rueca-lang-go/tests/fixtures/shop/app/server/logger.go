package server

// Logger logs messages.
type Logger interface {
	Log(msg string)
}

// ReadLogger also reads.
type ReadLogger interface {
	Logger
	Read() string
}

// Base is embedded by Server.
type Base struct{}

// Log logs with a value receiver.
func (b Base) Log(msg string) {}

// Read reads.
func (b *Base) Read() string { return "" }

// Number is a constraint, not an interface to implement.
type Number interface {
	~int | ~int64
}
