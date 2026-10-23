"""Invoice service module."""
from __future__ import annotations

import os.path
import numpy as np
from typing import TYPE_CHECKING, Optional
from . import models
from ..core.base import BaseService, helper as h
from .utils import *

if TYPE_CHECKING:
    from .repo import Repository

__all__ = ["InvoiceService", "issue_all", "MAX_RETRIES"]

MAX_RETRIES = 3
default_timeout: float = 2.5
_cache, (left, right) = {}, (1, 2)


class InvoiceError(Exception):
    pass


@dataclass(frozen=True)
class InvoiceService(BaseService, models.Mixin, metaclass=Meta):
    """Issues invoices.

    Second paragraph.
    """

    currency: str = "MXN"
    rate = DEFAULT_RATE

    def __init__(self, repo: Repository, timeout=default_timeout):
        self.repo = repo
        self.timeout: int = timeout
        self.repo = None

    @app.route("/x")
    @staticmethod
    async def issue(
        self,
        invoice: Optional[models.Invoice],  # the invoice
        *args: list[Item],
    ) -> Receipt:
        def inner(x):
            return x + total

        total = compute(invoice, os.path.sep)
        if not invoice:
            raise InvoiceError("missing")
        self.repo.save(invoice)
        self.count += 1
        result = [n for n in invoice.lines if n.ok]
        print(f"{settings.DEBUG} {result}")
        return helper.wrap(inner(total), key=CONFIG)

    class Nested:
        def m(self):
            pass


def issue_all(items):
    service = InvoiceService(get_repo())
    for item in items:
        service.issue(item)
    get_client().session.close()
    raise ValueError


def _private():
    pass


def test_issue():
    assert issue_all([]) is None


class TestService:
    def test_one(self):
        pass

    def testTwo(self):
        pass

    def helper(self):
        pass
