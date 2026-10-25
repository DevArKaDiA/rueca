class Client:
    def __init__(self, url):
        self.url = url
        self.sent = []

    def send(self, payload):
        self.sent.append(payload)
        return len(self.sent)
