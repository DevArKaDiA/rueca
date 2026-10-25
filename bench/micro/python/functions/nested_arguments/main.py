def parse(raw):
    return raw.strip()


def validate(text):
    return len(text) > 0


def report(ok, text):
    print(ok, text)
    return ok


def run(raw):
    text = parse(raw)
    return report(validate(parse(raw)), text)


if __name__ == "__main__":
    run(parse(" x "))
