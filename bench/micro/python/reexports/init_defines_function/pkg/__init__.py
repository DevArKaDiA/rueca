from .impl import build


def version():
    major, minor = 1, 2
    return f"{major}.{minor}"


def banner(name):
    label = build(name)
    return f"{label} {version()}"
