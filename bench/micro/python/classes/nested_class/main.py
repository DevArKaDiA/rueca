class Outer:
    class Inner:
        def ping(self):
            reply = "pong"
            return reply

    def make(self):
        inner = Outer.Inner()
        return inner.ping()


def build():
    inner = Outer.Inner()
    outer = Outer()
    return inner.ping(), outer.make()
