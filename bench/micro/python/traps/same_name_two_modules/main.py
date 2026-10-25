import beta
from alpha import run


def first(job):
    result = run(job)
    return result


def second(job):
    result = beta.run(job)
    return result


def third(job):
    from beta import run

    return run(job)
