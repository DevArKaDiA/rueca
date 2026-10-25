import models
from models import User


def header(user: User):
    title = user.display()
    return f"<h1>{title}</h1>"


def footer(user: "models.User"):
    name = user.display()
    return f"<p>{name}</p>"
