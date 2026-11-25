struct Parser {
    func parse() -> Int { 0 }
}

func run() -> Int {
    let p = Parser()
    return p.parse()
}

print(run())
