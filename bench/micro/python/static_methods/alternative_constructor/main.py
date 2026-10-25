class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def validate(self):
        ok = self.x >= 0 and self.y >= 0
        return ok

    @classmethod
    def from_dict(cls, data) -> "Point":
        obj = cls(data["x"], data["y"])
        obj.validate()
        return obj

    def norm(self):
        value = (self.x**2 + self.y**2) ** 0.5
        return value


def load(data):
    p = Point.from_dict(data)
    return p.norm()
