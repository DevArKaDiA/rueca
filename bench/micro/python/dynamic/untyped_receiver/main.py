class Handler:
    def run(self, event):
        result = "handled " + event
        return result


def parse(text):
    value = text.strip()
    return value


def process(handler, event):
    result = handler.run(event)
    return result


def tokenize(parser, text):
    tokens = parser.parse(text)
    return tokens
