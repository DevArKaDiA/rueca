def score(item):
    value = len(item)
    return value


def notify(name):
    message = "done " + name
    return message


def clean(text):
    stripped = text.strip()
    return stripped


def is_valid(item):
    ok = len(item) > 0
    return ok


def key_of(item):
    key = f"k{item}"
    return key


def rank(items):
    ordered = sorted(items, key=lambda item: score(item))
    callback = lambda: notify("rank")
    callback()
    return ordered


def tabulate(items):
    scores = [score(x) for x in items if is_valid(x)]
    table = {key_of(x): x for x in items}
    total = sum(score(x) for x in items)
    return scores, table, total


TRANSFORM = lambda text: clean(text)
