import pkg


def show(name):
    text = pkg.banner(name)
    return text + pkg.version()
