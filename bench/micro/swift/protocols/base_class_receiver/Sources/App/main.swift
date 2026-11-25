class Animal {
    func speak() -> String { "" }
}

final class Cat: Animal {
    override func speak() -> String { "meow" }
}

func talk(_ a: Animal) -> String {
    return a.speak()
}

print(talk(Cat()))
