ROWS = {}


def save(key, row):
    ROWS[key] = row
    return key


def load(key):
    row = ROWS.get(key)
    return row
