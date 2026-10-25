import json
import os
from collections import OrderedDict


def dump(data):
    text = json.dumps(data)
    print(len(text), os.getcwd())
    return sorted(data)


def load(text):
    raw = json.loads(text)
    return OrderedDict(raw)


def keys(data):
    names = list(data.keys())
    names.sort()
    return ",".join(names).upper()
