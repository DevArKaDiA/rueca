import pkg
from pkg import assist


def run(value):
    result = assist(value)
    return result


def run_attr(value):
    result = pkg.assist(value)
    return result
