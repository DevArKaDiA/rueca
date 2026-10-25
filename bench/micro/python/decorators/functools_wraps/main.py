from decos import timed


def crunch(n):
    total = sum(range(n))
    return total


@timed
def work(n):
    value = crunch(n)
    return value * 2


def main():
    out = work(10)
    return out
