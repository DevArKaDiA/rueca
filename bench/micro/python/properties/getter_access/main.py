from functools import cached_property


class Order:
    def __init__(self, items):
        self.items = items

    def subtotal(self):
        value = sum(self.items)
        return value

    @property
    def total(self):
        base = self.subtotal()
        return base * 1.16

    @cached_property
    def summary(self):
        lines = len(self.items)
        return {"lines": lines, "total": self.total}

    def render(self):
        data = self.summary
        return str(data["total"])


def invoice():
    o = Order([1, 2])
    amount = o.total
    return amount, o.subtotal(), o.summary, o.render()
