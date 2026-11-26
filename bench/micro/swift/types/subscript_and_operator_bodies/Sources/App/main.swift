struct Grid {
    var cells: [Int] = [0, 0]

    func clamp(_ i: Int) -> Int { max(0, min(i, 1)) }

    subscript(i: Int) -> Int {
        return cells[clamp(i)]
    }

    static func + (lhs: Grid, rhs: Grid) -> Grid {
        return merge(lhs, rhs)
    }

    static func merge(_ a: Grid, _ b: Grid) -> Grid { a }
}

func run() -> Int {
    let g = Grid() + Grid()
    return g[3]
}

print(run())
