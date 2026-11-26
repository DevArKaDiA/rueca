actor Bank {
    var total = 0

    func deposit(_ n: Int) {
        total += n
        log()
    }

    func log() {}
}

func run() async {
    let b = Bank()
    await b.deposit(1)
}

Task { await run() }
