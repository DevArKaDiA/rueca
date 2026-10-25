def audit(func):
    def wrapper(self, *args):
        note(func.__name__)
        return func(self, *args)

    return wrapper


def note(name):
    entry = f"call {name}"
    return entry


class Account:
    def __init__(self):
        self.balance = 0

    @audit
    def deposit(self, amount):
        self.balance += amount
        return self.check()

    def check(self):
        ok = self.balance >= 0
        return ok

    def top_up(self):
        self.deposit(10)
        return self.balance


def main():
    acc = Account()
    acc.deposit(5)
    return acc.top_up()
