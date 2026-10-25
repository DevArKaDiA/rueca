class Model:
    def __init__(self, ident):
        self.ident = ident

    def save(self):
        key = f"row:{self.ident}"
        return key
