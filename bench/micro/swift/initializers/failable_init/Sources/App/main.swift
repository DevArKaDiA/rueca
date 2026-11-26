struct Version {
    let major: Int

    init?(string: String) {
        guard let n = Int(string) else { return nil }
        major = n
    }
}

func parse() -> Int {
    return Version(string: "3")?.major ?? 0
}

print(parse())
