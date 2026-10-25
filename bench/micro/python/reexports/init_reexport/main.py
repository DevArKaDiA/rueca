import pkg
from pkg import Client, connect


def start(url):
    handle = connect(url)
    return handle


def start_again(url):
    handle = pkg.connect(url)
    return handle


def push(url, payload):
    c = Client(url)
    return c.send(payload)
