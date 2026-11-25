func helper() {}

func run() {
    let action = {
        helper()
    }
    action()
}

run()
