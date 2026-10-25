def factorial(n):
    if n <= 1:
        return 1
    return n * factorial(n - 1)


def is_even(n):
    if n == 0:
        return True
    return is_odd(n - 1)


def is_odd(n):
    if n == 0:
        return False
    return is_even(n - 1)
