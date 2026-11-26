struct Query {
    func run() -> Int { 0 }
}

func makeQuery() -> Query {
    return Query()
}

func execute() -> Int {
    return makeQuery().run()
}

print(execute())
