REGISTRY = {}


def add_to_registry(name, cls):
    REGISTRY[name] = cls
    return cls


def register(cls):
    add_to_registry(cls.__name__, cls)
    return cls


@register
class Plugin:
    def run(self):
        status = "ran"
        return status


def main():
    p = Plugin()
    return p.run()
