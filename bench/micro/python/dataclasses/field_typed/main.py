from dataclasses import dataclass, field
from typing import List


@dataclass
class Customer:
    email_address: str

    def email(self, text):
        message = (self.email_address, text)
        return message


@dataclass
class Order:
    customer: Customer
    lines: List[str] = field(default_factory=list)

    def notify(self):
        sent = self.customer.email("shipped")
        return sent


def ship(order: Order):
    order.notify()
    return order.customer.email("thanks")
