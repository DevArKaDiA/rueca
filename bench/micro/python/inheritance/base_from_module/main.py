from models.user import User


def create(ident):
    u = User(ident)
    u.rename("x")
    return u.save()
