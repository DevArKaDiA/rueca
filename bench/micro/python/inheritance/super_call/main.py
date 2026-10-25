class Base:
    def __init__(self, name):
        self.name = name

    def save(self):
        row = {"name": self.name}
        return row

    def label(self):
        text = "base"
        return text


class Child(Base):
    def __init__(self, name, age):
        super().__init__(name)
        self.age = age

    def save(self):
        row = super().save()
        row["age"] = self.age
        return row

    def label(self):
        first = super(Child, self).label()
        return first + Base.label(self)


def store():
    c = Child("ana", 3)
    return c.save()
