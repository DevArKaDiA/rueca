public struct Point: Shape {
    public var x: Double
    public var y: Double

    public init(x: Double, y: Double) {
        self.x = x
        self.y = y
    }

    public func area() -> Double {
        return 0
    }

    public func length() -> Double {
        return (x * x + y * y).squareRoot()
    }

    public func describe() -> String {
        return "(\(x), \(y))"
    }

    public static func origin() -> Point {
        return Point(x: 0, y: 0)
    }
}
