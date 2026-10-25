class Base:
    def __init__(self, ident):
        self.ident = ident

    def describe(self):
        text = f"#{self.ident}"
        return text


class Child(Base):
    def tag(self):
        label = "child"
        return label


def make():
    c = Child(7)
    c.tag()
    return c.describe()
