func double(_ x: Int) -> Int { x * 2 }

func run() -> [Int] {
    return [1, 2].map(double)
}

print(run())
