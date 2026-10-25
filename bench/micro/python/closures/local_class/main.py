def helper(x):
    value = x + 1
    return value


def make(n):
    class Local:
        def go(self):
            result = helper(n)
            return result

    obj = Local()
    return obj.go()
