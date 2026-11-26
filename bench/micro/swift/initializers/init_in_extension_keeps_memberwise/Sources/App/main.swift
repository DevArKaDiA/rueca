struct Point {
    var x: Double
    var y: Double
}

func run() -> Double {
    let a = Point(x: 1, y: 2)
    let b = Point(radius: 1)
    return a.x + b.y
}

print(run())
