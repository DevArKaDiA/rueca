from typing import Optional


class Repo:
    def save(self, item):
        rows = [item]
        return rows


class Cache:
    def save(self, item):
        rows = {item: True}
        return rows


class Logger:
    def info(self, text):
        line = "INFO " + text
        return line


def lookup(registry, name):
    found = registry[name]
    return found


def persist(repo: Repo, item):
    repo.save(item)
    return item


def persist_from(registry, item):
    cache: Cache = lookup(registry, "cache")
    return cache.save(item)


def log(logger: Optional[Logger] = None, fallback: "Logger | None" = None):
    if logger is not None:
        logger.info("start")
    if fallback:
        fallback.info("fallback")
    return True
