def clamp(value):
    bounded = max(0, value)
    return bounded


def make_adder(n):
    def add(x):
        total = x + n
        return clamp(total)

    return add


def main():
    add_two = make_adder(2)
    result = add_two(5)
    return result
