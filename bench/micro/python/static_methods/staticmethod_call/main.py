class MathUtil:
    @staticmethod
    def square(x):
        value = x * x
        return value

    @staticmethod
    def cube(x):
        value = MathUtil.square(x) * x
        return value

    def both(self, x):
        a = self.square(x)
        return a + self.cube(x)


def compute(x):
    a = MathUtil.square(x)
    m = MathUtil()
    return a + m.cube(x) + m.both(x)
