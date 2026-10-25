class Vector:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __add__(self, other):
        return Vector(self.x + other.x, self.y + other.y)

    def __len__(self):
        size = 2
        return size

    def __iter__(self):
        yield self.x
        yield self.y

    def scaled(self, k):
        factor = k
        return Vector(self.x * factor, self.y * factor)


class Transaction:
    def __enter__(self) -> "Transaction":
        self.open = True
        return self

    def __exit__(self, *exc):
        self.open = False
        return False

    def execute(self, sql):
        rows = [sql]
        return rows


def combine():
    total = Vector(0, 0)
    other = Vector(1, 1)
    summed = total + other
    for part in total:
        print(part, len(summed))
    return total.__add__(other.scaled(2))


def transfer():
    with Transaction() as tx:
        tx.execute("UPDATE")
    return True
