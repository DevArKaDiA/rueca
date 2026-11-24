"""Benchmark arms."""

from __future__ import annotations

from .base import Arm
from .cbm import CbmArm
from .grep import GrepArm
from .rueca import RuecaArm, RuecaConfig
from .vector import VectorArm, VectorConfig

# `--arm all` and every suite: the original arms.
ARM_NAMES = ("grep", "cbm", "rueca")
# Rueca with vectors (RFC 0007): `--arm rueca-hybrid`, or `--arm rueca` with
# RUECA_BENCH_RUECA_MODE=hybrid|semantic (`canonical_arm`).
RUECA_MODE_ARMS = ("rueca-hybrid", "rueca-semantic")
# Localization-only arms (S1); named
# explicitly (`--arm vector`), never part of `all`: embedding the corpus
# costs hours on a laptop.
LOCALIZATION_ONLY = ("vector", *RUECA_MODE_ARMS)
S1_ARM_NAMES = ARM_NAMES + LOCALIZATION_ONLY


def _rueca_config(name: str) -> RuecaConfig:
    return RuecaConfig.from_env(mode=name.removeprefix("rueca-"))


def canonical_arm(name: str) -> str:
    """`rueca` under RUECA_BENCH_RUECA_MODE=hybrid|semantic is the
    `rueca-<mode>` arm; every other name is itself."""
    return RuecaConfig.from_env().arm_name if name == "rueca" else name


def make_arm(name: str, call_timeout: float = 120.0, index_timeout: float = 1800.0) -> Arm:
    if name == "grep":
        return GrepArm()
    if name == "cbm":
        return CbmArm(call_timeout, index_timeout)
    if name == "rueca":
        return RuecaArm(call_timeout, index_timeout)
    if name == "vector":
        return VectorArm()
    if name in RUECA_MODE_ARMS:
        return RuecaArm(call_timeout, index_timeout, _rueca_config(name))
    raise ValueError(f"unknown arm {name}")


def run_label(name: str) -> str:
    """Directory of an arm's cached outputs under `runs/<suite>/`: the arm
    name, plus the sweep setting for the vector arm (`vector/<tag>`) and
    Rueca's vector modes (`rueca-hybrid/<tag>`)."""
    if name == "vector":
        return f"vector/{VectorConfig.from_env().tag()}"
    if name in RUECA_MODE_ARMS:
        return _rueca_config(name).run_label()
    return name
