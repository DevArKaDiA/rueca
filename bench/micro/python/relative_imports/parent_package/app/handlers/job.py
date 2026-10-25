from ..core.engine import run as run_engine
from ..models import user
from ..util import fmt


def handle(job):
    result = run_engine(job)
    return fmt(result)


def signup(name):
    record = user.create(name)
    return fmt(record["name"])
