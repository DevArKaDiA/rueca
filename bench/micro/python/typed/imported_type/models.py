class User:
    def __init__(self, name):
        self.name = name

    def display(self):
        text = self.name.title()
        return text
