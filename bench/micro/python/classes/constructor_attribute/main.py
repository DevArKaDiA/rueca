class Repo:
    def save(self, item):
        stored = [item]
        return stored


class Service:
    def __init__(self):
        self.repo = Repo()

    def handle(self, item):
        result = self.repo.save(item)
        return result
