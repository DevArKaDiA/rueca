final class Store {
    func save() {}
}

final class Service {
    let store = Store()

    func run() {
        store.save()
    }
}

Service().run()
