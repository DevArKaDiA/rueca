extension String {
    func shouted() -> String {
        return uppercased() + "!"
    }
}

func greet(_ name: String) -> String {
    return name.shouted()
}

func literal() -> String {
    return "hi".shouted()
}

print(greet("a") + literal())
