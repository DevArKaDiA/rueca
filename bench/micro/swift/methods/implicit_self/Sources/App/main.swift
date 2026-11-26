struct Counter {
    var n = 0

    mutating func bump() {
        add(1)
    }

    mutating func add(_ k: Int) {
        n += k
    }
}

var c = Counter()
c.bump()
