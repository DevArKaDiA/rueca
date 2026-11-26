protocol Describable {
    var name: String { get }
}

extension Describable {
    func describe() -> String {
        return "<" + name + ">"
    }
}

struct User: Describable {
    let name: String
}

func show(_ u: User) -> String {
    return u.describe()
}

print(show(User(name: "a")))
