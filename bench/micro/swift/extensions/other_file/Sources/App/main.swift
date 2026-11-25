final class Account {
    var balance = 0

    func deposit(_ amount: Int) {
        balance += amount
        audit()
    }
}

Account().deposit(1)
