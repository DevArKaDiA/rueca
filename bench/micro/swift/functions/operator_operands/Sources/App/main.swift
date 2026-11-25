func base() -> Int { 1 }

func extra() -> Int { 2 }

func check() -> Bool { true }

func run() -> Int {
    let total = base() + extra()
    if !check() {
        return 0
    }
    return total * extra()
}

print(run())
