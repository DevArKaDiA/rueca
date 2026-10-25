from .base import Model


class User(Model):
    def rename(self, name):
        self.name = name
        return self.save()
