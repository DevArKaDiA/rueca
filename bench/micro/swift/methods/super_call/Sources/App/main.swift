class Base {
    func setup() {}
}

final class Child: Base {
    override func setup() {
        super.setup()
        configure()
    }

    func configure() {}
}

Child().setup()
