protocol Shape {
    func area() -> Double
}

struct Square: Shape {
    let side: Double

    func area() -> Double { side * side }
}

func total(_ shapes: [Shape]) -> Double {
    var sum = 0.0
    for s in shapes {
        sum += s.area()
    }
    return sum
}

func one(_ s: Shape) -> Double {
    return s.area()
}

print(total([Square(side: 1)]) + one(Square(side: 2)))
