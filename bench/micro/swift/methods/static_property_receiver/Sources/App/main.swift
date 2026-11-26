final class Settings {
    static let shared = Settings()

    func load() {}
}

func run() {
    Settings.shared.load()
}

run()
