struct Box<T> {
    let value: T

    func unwrap() -> T { value }
}

func open<T>(_ b: Box<T>) -> T {
    return b.unwrap()
}

print(open(Box(value: 1)))
