struct Money {
    var cents: Int
}

extension Money {
    func doubled() -> Money {
        return Money(cents: cents * 2)
    }

    func formatted() -> String {
        return String(doubled().cents)
    }
}

print(Money(cents: 1).formatted())
