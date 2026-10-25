def save(item):
    stored = [item]
    return stored


class Repo:
    def save(self, item):
        stored = {item: True}
        return stored

    def flush(self, item):
        save(item)
        return self.save(item)


def flush(item):
    repo = Repo()
    return repo.flush(item)
