from __future__ import annotations


def make_builder() -> Builder:
    b = Builder()
    return b


def order():
    b = make_builder()
    status = b.named("box").build().ship()
    return status


class Product:
    def __init__(self, name):
        self.name = name

    def ship(self):
        status = "shipped " + self.name
        return status


class Builder:
    def __init__(self):
        self.name = ""

    def named(self, name) -> Builder:
        self.name = name
        return self

    def build(self) -> Product:
        product = Product(self.name)
        return product
