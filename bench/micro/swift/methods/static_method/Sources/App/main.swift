struct Factory {
    static func make() -> Int {
        return Self.seed() + Factory.seed()
    }

    static func seed() -> Int {
        return 1
    }
}

func run() -> Int {
    return Factory.make()
}

print(run())
