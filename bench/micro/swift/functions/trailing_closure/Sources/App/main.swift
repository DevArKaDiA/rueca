func helper() {}

func perform(_ body: () -> Void) {
    body()
}

func run() {
    perform {
        helper()
    }
}

run()
