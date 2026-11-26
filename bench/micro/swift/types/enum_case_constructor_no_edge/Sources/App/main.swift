enum Result2 {
    case ok(Int)
    case failed(String)

    static func make(_ n: Int) -> Result2 {
        return n > 0 ? .ok(n) : .failed("neg")
    }
}

func run() -> Result2 {
    return Result2.make(1)
}

print(run())
