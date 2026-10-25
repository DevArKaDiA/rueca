class Counter:
    def __init__(self):
        self.value = 0
        self.reset()

    def reset(self):
        self.value = 0

    def _bump(self, n):
        self.value += n
        return self.value

    def increment(self):
        self._bump(1)
        return self.value


def use():
    c = Counter()
    c.increment()
    return c.value
