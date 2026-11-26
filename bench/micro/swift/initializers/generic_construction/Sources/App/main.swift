struct Wrapper<T> {
    let value: T

    init(_ value: T) {
        self.value = value
    }

    func get() -> T { value }
}

func run() -> Int {
    let w = Wrapper<Int>(5)
    return w.get()
}

print(run())
