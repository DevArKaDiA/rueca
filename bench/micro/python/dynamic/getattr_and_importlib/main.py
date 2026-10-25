import importlib


class Runner:
    def run(self):
        status = "ran"
        return status


def run_job():
    status = "job"
    return status


def by_name(r: Runner, name):
    method = getattr(r, name)
    return method()


def by_literal(r: Runner):
    result = getattr(r, "run")()
    return result


def by_globals():
    result = globals()["run_job"]()
    return result


def by_module(name):
    module = importlib.import_module(name)
    return module.run_job()
