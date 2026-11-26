func log(_ message: String, level: Int = 0) {}

func log(_ message: String, tag: String) {}

func run() {
    log("a")
    log("b", level: 2)
}

run()
