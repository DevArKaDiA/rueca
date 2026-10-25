from .repo import load
from .repo import save as persist


def rename(key, name):
    row = load(key)
    row["name"] = name
    return persist(key, row)
