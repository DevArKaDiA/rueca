package app;

class Account {
    void deposit(int amount) {
    }
}

class Bank {
    void pay(Account account) {
        account.deposit(10);
    }
}
