package server

// Context is a request context.
type Context struct {
	Path string
}

// Handle handles one request.
func (s *Server) Handle(c *Context) {
	if c != nil {
		c.Reset()
	}
	s.Start()
	helper(c)
	cb := func(ctx *Context) { ctx.Reset() }
	cb(c)
}

// Reset clears the context.
func (c *Context) Reset() { c.Path = "" }

func helper(c *Context) {}
