struct Circle {
    let r: Double

    var area: Double {
        return 3.14 * r * r
    }

    func describe() -> String {
        return String(area)
    }
}

print(Circle(r: 1).describe())
