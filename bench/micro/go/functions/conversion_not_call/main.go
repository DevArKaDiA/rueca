package main

type Celsius float64

func (c Celsius) Fahrenheit() float64 {
	return float64(c)*9/5 + 32
}

func run() float64 {
	c := Celsius(10)
	return c.Fahrenheit()
}

func main() {
	_ = run()
}
