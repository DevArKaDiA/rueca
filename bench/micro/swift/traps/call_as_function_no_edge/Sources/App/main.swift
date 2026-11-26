struct Adder {
    func callAsFunction(_ x: Int) -> Int { x + 1 }
}

func run() -> Int {
    let add = Adder()
    return add(1)
}

print(run())
