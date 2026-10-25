import textutil
import textutil as tu


def render(title):
    slug = textutil.slugify(title)
    return "/" + slug


def raw(title):
    value = tu.normalize(title)
    return value


def lazy(title):
    import textutil as late

    return late.slugify(title)
