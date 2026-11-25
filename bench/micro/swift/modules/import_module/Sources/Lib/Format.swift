public func format(_ n: Int) -> String {
    return pad(String(n))
}

func pad(_ s: String) -> String {
    return " " + s
}

public struct Formatter {
    public init() {}

    public func run(_ n: Int) -> String {
        return format(n)
    }
}
