"""Benchmark arms."""

from __future__ import annotations

from .base import Arm
from .cbm import CbmArm
from .grep import GrepArm
from .telar import TelarArm

ARM_NAMES = ("grep", "cbm", "telar")


def make_arm(name: str, call_timeout: float = 120.0, index_timeout: float = 1800.0) -> Arm:
    if name == "grep":
        return GrepArm()
    if name == "cbm":
        return CbmArm(call_timeout, index_timeout)
    if name == "telar":
        return TelarArm(call_timeout, index_timeout)
    raise ValueError(f"unknown arm {name}")
