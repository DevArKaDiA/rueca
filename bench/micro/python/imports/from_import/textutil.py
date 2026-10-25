def normalize(text):
    cleaned = text.strip()
    return cleaned.lower()


def slugify(text):
    base = normalize(text)
    return base.replace(" ", "-")


def unused(text):
    return text
