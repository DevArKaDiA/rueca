import Foundation

extension Point {
    public init(polar r: Double, angle: Double) {
        self.init(x: r * cos(angle), y: r * sin(angle))
    }

    public func scaled(by k: Double) -> Point {
        let p = Point(x: x * k, y: y * k)
        return p.normalized()
    }

    func normalized() -> Point {
        let len = length()
        return Point(x: x / len, y: y / len)
    }
}

extension String {
    public func shouted() -> String {
        return uppercased()
    }
}

public func makeCanvas() -> Canvas {
    let home = ProcessInfo.processInfo.environment["CANVAS_HOME"]
    _ = home
    return Canvas(origin: .origin())
}
