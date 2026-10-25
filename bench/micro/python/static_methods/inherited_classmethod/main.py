class Base:
    @classmethod
    def name(cls):
        label = "base"
        return label

    @classmethod
    def create(cls):
        label = cls.name()
        return label

    @staticmethod
    def version():
        number = 1
        return number


class Derived(Base):
    @classmethod
    def name(cls):
        label = "derived"
        return label


def main():
    a = Derived.create()
    b = Derived.version()
    return a, b, Derived.name()
