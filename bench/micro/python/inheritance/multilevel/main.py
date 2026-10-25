class A:
    def ping(self):
        reply = "pong"
        return reply


class B(A):
    def middle(self):
        reply = self.ping()
        return reply


class C(B):
    def top(self):
        first = self.middle()
        return first + self.ping()


def run():
    c = C()
    c.ping()
    return c.top()
