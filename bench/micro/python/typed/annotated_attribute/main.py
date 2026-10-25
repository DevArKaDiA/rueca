class Mailer:
    def send(self, to, text):
        envelope = (to, text)
        return envelope


class Notifier:
    mailer: Mailer

    def __init__(self, mailer):
        self.mailer = mailer

    def alert(self, to):
        sent = self.mailer.send(to, "alert")
        return sent
