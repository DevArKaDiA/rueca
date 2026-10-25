class Walker:
    def move(self):
        how = "walk"
        return how

    def rest(self):
        how = "sit"
        return how


class Swimmer:
    def move(self):
        how = "swim"
        return how

    def dive(self):
        how = "dive"
        return how


class Duck(Walker, Swimmer):
    pass


class Fish(Swimmer, Walker):
    pass


def day():
    d = Duck()
    f = Fish()
    d.move()
    d.dive()
    f.move()
    return f.rest()
