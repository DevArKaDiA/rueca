final class Button {
    var onTap: () -> Void = {}

    func tap() {
        onTap()
    }
}

func handle() {}

func run() {
    let b = Button()
    b.onTap = { handle() }
    b.tap()
}

run()
