protocol Runner {
    func run() -> Int
}

struct Fast: Runner {
    func run() -> Int { 1 }
}

func execute<R: Runner>(_ r: R) -> Int {
    return r.run()
}

print(execute(Fast()))
