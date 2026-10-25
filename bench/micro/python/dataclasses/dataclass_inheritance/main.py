from dataclasses import dataclass


@dataclass
class Event:
    name: str

    def describe(self):
        text = "event " + self.name
        return text


@dataclass(eq=False)
class Click(Event):
    x: int = 0

    def position(self):
        where = (self.x,)
        return where


def handle():
    c = Click("click", 3)
    c.position()
    return c.describe()
