"""Invoices: a small call graph for the golden transcripts."""


def compute_tax(amount: float, rate: float = 0.16) -> float:
    """Tax owed on an amount."""
    return round(amount * rate, 2)


def apply_discount(amount: float, percent: float) -> float:
    """Amount after a percentage discount."""
    return amount * (1 - percent / 100)


def total_due(amount: float, percent: float) -> float:
    """Discounted amount plus its tax."""
    net = apply_discount(amount, percent)
    return net + compute_tax(net)


class InvoiceService:
    """Issues invoices."""

    def __init__(self, prefix: str) -> None:
        self.prefix = prefix

    def issue(self, number: int, amount: float) -> str:
        """Issue one invoice and return its label."""
        due = total_due(amount, 0)
        return format_label(self.prefix, number, due)


def format_label(prefix: str, number: int, due: float) -> str:
    """GOLDEN_MARKER_7F3A label format."""
    return f"{prefix}-{number:05d}: {due:.2f}"
