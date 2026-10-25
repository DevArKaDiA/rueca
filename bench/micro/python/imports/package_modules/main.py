import storage.backends.memory as mem
import storage.disk
from storage import mailer
from storage.backends import memory


def save(key, value):
    storage.disk.write(key, value)
    return mem.put(key, value)


def save_and_notify(key, value):
    memory.put(key, value)
    return mailer.send(key, "saved")
