class Animal {}

final class Dog: Animal {
    func bark() {}
}

func handle(_ a: Animal) {
    (a as? Dog)?.bark()
    if let d = a as? Dog {
        d.bark()
    }
}

handle(Dog())
