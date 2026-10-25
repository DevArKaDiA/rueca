from textutil import normalize
from textutil import slugify as make_slug


def render(title):
    slug = make_slug(title)
    return "/" + normalize(slug)


def lazy(title):
    from textutil import slugify

    return slugify(title)
