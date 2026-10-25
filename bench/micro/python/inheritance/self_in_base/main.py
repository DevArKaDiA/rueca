class Pipeline:
    def step(self):
        name = "base"
        return name

    def execute(self):
        result = self.step()
        return result


class Custom(Pipeline):
    def step(self):
        name = "custom"
        return name

    def execute_twice(self):
        first = self.execute()
        return first + self.step()


def go():
    p = Custom()
    p.execute()
    return p.step()
