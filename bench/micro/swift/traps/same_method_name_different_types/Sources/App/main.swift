struct Cat {
    func run() -> Int { 1 }
}

struct Dog {
    func run() -> Int { 2 }
}

func race(_ c: Cat, _ d: Dog) -> Int {
    return c.run() + d.run()
}

func cats(_ c: Cat) -> Int {
    return c.run()
}

print(race(Cat(), Dog()) + cats(Cat()))
