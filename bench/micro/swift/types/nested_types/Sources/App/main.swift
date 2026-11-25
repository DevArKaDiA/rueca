struct Outer {
    struct Inner {
        func ping() -> Int { 1 }
    }

    func call() -> Int {
        return Inner().ping()
    }
}

func run() -> Int {
    return Outer.Inner().ping() + Outer().call()
}

print(run())
