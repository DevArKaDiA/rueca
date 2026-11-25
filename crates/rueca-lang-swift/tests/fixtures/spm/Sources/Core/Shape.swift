/// Something that can be drawn.
public protocol Shape {
    func area() -> Double
    func describe() -> String
}

public final class Canvas {
    private var shapes: [Shape] = []
    public let origin: Point

    public init(origin: Point) {
        self.origin = origin
    }

    public func add(_ shape: Shape) {
        shapes.append(shape)
        log(shape.describe())
    }

    public func total() -> Double {
        var sum = 0.0
        for s in shapes {
            sum += s.area()
        }
        return sum + origin.length()
    }

    func log(_ message: String) {
        print(message)
    }
}
