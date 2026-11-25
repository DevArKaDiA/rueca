func fact(_ n: Int) -> Int {
    return n <= 1 ? 1 : n * fact(n - 1)
}

func countdown(_ n: Int) {
    if n > 0 {
        countdown(n - 1)
    }
}

print(fact(3))
countdown(2)
