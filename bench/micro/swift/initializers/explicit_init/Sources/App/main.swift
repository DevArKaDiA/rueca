struct Box {
    let value: Int

    init(value: Int) {
        self.value = value
        validate()
    }

    func validate() {}
}

func make() -> Box {
    return Box(value: 1)
}

print(make().value)
