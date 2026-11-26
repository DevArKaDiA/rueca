func fetch() async -> Int { 1 }

func run() async -> Int {
    let a = await fetch()
    return a
}

Task { _ = await run() }
