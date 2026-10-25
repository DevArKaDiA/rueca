package main

type Point struct {
	x, y int
}

func (p Point) Sum() int {
	return p.x + p.y
}

func (p *Point) Move(dx int) {
	p.x += dx
}

func run() int {
	p := &Point{x: 1, y: 2}
	p.Move(3)
	return p.Sum()
}

func main() {
	_ = run()
}
