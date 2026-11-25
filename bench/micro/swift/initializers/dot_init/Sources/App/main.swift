struct Config {
    let debug: Bool

    init(debug: Bool) {
        self.debug = debug
    }
}

func make() -> Config {
    let c: Config = .init(debug: true)
    return c
}

print(make().debug)
