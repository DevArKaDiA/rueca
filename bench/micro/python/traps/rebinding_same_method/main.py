class Reader:
    def run(self):
        data = "read"
        return data


class Writer:
    def run(self):
        data = "write"
        return data


def read_only():
    r = Reader()
    return r.run()


def both():
    x = Reader()
    x.run()
    x = Writer()
    return x.run()
