final class Handle {
    deinit {
        close()
    }

    func close() {}
}

func run() {
    _ = Handle()
}

run()
