import functools
import time


def emit(name, elapsed):
    line = f"{name} took {elapsed:.3f}s"
    print(line)


def timed(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        start = time.perf_counter()
        result = func(*args, **kwargs)
        emit(func.__name__, time.perf_counter() - start)
        return result

    return wrapper
