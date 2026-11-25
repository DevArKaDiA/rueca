class Shape {
    let sides: Int

    init(sides: Int) {
        self.sides = sides
    }
}

final class Square: Shape {
    let side: Double

    init(side: Double) {
        self.side = side
        super.init(sides: 4)
    }

    convenience init() {
        self.init(side: 1)
    }
}

print(Square().side)
