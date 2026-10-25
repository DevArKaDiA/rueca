from dataclasses import dataclass


@dataclass
class Point:
    x: float
    y: float

    def __post_init__(self):
        self.check()

    def check(self):
        ok = self.x >= 0
        return ok

    def norm(self):
        value = (self.x**2 + self.y**2) ** 0.5
        return value


def length():
    p = Point(3.0, 4.0)
    return p.norm()
