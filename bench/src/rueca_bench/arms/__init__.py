"""Benchmark arms."""

from __future__ import annotations

from .base import Arm
from .cbm import CbmArm
from .grep import GrepArm
from .rueca import RuecaArm
from .vector import VectorArm, VectorConfig

# `--arm all` and every suite: the original arms.
ARM_NAMES = ("grep", "cbm", "rueca")
# Localization-only arms (S1); named
# explicitly (`--arm vector`), never part of `all`: embedding the corpus
# costs hours on a laptop.
LOCALIZATION_ONLY = ("vector",)
S1_ARM_NAMES = ARM_NAMES + LOCALIZATION_ONLY


def make_arm(name: str, call_timeout: float = 120.0, index_timeout: float = 1800.0) -> Arm:
    if name == "grep":
        return GrepArm()
    if name == "cbm":
        return CbmArm(call_timeout, index_timeout)
    if name == "rueca":
        return RuecaArm(call_timeout, index_timeout)
    if name == "vector":
        return VectorArm()
    raise ValueError(f"unknown arm {name}")


def run_label(name: str) -> str:
    """Directory of an arm's cached outputs under `runs/<suite>/`: the arm
    name, plus the sweep setting for the vector arm (`vector/<tag>`)."""
    if name == "vector":
        return f"vector/{VectorConfig.from_env().tag()}"
    return name
