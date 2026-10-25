def default_name():
    name = "anon"
    return name


class User:
    def __init__(self, name=None):
        self.name = name or default_name()

    def greet(self):
        text = "hi " + self.name
        return text


def welcome():
    u = User("ana")
    return u.greet()
