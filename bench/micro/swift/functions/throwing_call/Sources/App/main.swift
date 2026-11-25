struct LoadError: Error {}

func load() throws -> Int {
    throw LoadError()
}

func run() {
    do {
        _ = try load()
    } catch {
        print(error)
    }
}

run()
