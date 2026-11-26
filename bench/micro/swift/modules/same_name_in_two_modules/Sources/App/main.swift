import Lib

func now() -> Int { 2 }

func run() -> Int {
    return now() + Lib.now()
}

print(run())
