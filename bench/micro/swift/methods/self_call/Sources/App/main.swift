final class Greeter {
    func greet() -> String {
        return self.name()
    }

    func name() -> String {
        return "x"
    }
}

print(Greeter().greet())
