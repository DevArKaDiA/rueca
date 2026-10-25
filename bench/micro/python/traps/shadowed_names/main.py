LOG = []


def print(*args):
    LOG.append(args)
    return len(LOG)


def fmt(value):
    text = str(value)
    return text


def render(value):
    def fmt(v):
        text = f"<{v}>"
        return text

    print("render")
    return fmt(value)


def plain(value):
    text = fmt(value)
    return text
