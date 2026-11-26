func process() -> Int { 0 }

struct Job {
    func process() -> Int { 1 }

    func work() -> Int {
        return process()
    }
}

func run(_ j: Job) -> Int {
    return process() + j.work()
}

print(run(Job()))
