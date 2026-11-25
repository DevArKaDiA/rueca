func compute() -> Int { 1 }

func run() -> Int {
    let compute = { 2 }
    return compute()
}

print(run())
