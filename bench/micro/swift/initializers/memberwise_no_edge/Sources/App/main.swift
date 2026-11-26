struct Point {
    var x: Int
    var y: Int

    func sum() -> Int { x + y }
}

func run() -> Int {
    let p = Point(x: 1, y: 2)
    return p.sum()
}

print(run())
