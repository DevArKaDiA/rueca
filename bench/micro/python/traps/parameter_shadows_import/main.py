import util
from util import parse


def with_param(parse, text):
    value = parse(text)
    return value


def with_local(text):
    parse = str.strip
    return parse(text)


def with_module_param(util, text):
    value = util.parse(text)
    return value


def normal(text):
    value = parse(text)
    return value
