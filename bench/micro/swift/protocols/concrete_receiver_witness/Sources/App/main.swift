protocol Loader {
    func load() -> Int
}

struct FileLoader: Loader {
    func load() -> Int { 2 }
}

func run() -> Int {
    let l = FileLoader()
    return l.load()
}

print(run())
