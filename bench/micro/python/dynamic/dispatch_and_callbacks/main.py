def handle_create(payload):
    action = "create"
    return action, payload


def handle_delete(payload):
    action = "delete"
    return action, payload


HANDLERS = {"create": handle_create, "delete": handle_delete}


def dispatch(kind, payload):
    handler = HANDLERS[kind]
    return handler(payload)


def dispatch_inline(kind, payload):
    result = HANDLERS.get(kind, handle_delete)(payload)
    return result


def apply(fn, value):
    result = fn(value)
    return result


def main():
    first = apply(handle_create, {})
    second = list(map(handle_delete, [{}]))
    return first, second
