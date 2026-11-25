func outer() -> Int {
    func inner(_ x: Int) -> Int {
        return x + 1
    }
    return inner(1)
}

print(outer())
