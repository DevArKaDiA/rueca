class Base:
    def setup(self):
        steps = ["base"]
        return steps


class Left(Base):
    def setup(self):
        steps = super().setup()
        return steps + ["left"]


class Right(Base):
    def setup(self):
        steps = super().setup()
        return steps + ["right"]


class Bottom(Left, Right):
    def setup(self):
        steps = super().setup()
        return steps + ["bottom"]


def build():
    b = Bottom()
    return b.setup()
