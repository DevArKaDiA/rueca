def helper(x):
    doubled = x * 2
    return doubled


def outer(items):
    def inner(x):
        value = helper(x)
        return value + sibling(x)

    def sibling(x):
        value = x - 1
        return value

    results = [inner(i) for i in items]
    return results
