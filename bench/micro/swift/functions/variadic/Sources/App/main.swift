func sum(_ xs: Int...) -> Int {
    return xs.reduce(0, +)
}

func run() -> Int {
    return sum(1, 2, 3)
}

print(run())
