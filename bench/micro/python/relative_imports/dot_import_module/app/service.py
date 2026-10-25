from . import repo
from . import repo as storage


def register(name):
    row = {"name": name}
    return repo.save(row)


def register_twice(name):
    storage.save({"name": name})
    return storage.save({"name": name.upper()})
