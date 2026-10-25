class Widget:
    def __init__(self, label):
        self.label = label

    def render(self):
        html = f"<b>{self.label}</b>"
        return html


class Plain:
    def render(self):
        html = "<p></p>"
        return html


def page():
    head = Widget("title").render()
    body = Plain().render()
    return head + body
