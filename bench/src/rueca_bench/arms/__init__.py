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
# RUECA_BENCH_RUECA_MODE=hybrid|semantic (`canonical_arm`); `rueca-lexical`
# is the lexical arm with a second stage on (RUECA_EXPAND, _CENTRALITY_BETA,
# _RERANK: RFC 0007 phase 2).
RUECA_MODE_ARMS = ("rueca-hybrid", "rueca-semantic", "rueca-lexical")
# Localization-only arms (S1 and the SWE-bench Lite diagnostic); named
# explicitly (`--arm vector`), never part of `all`: embedding the corpus
# costs hours on a laptop.
LOCALIZATION_ONLY = ("vector", *RUECA_MODE_ARMS)
S1_ARM_NAMES = ARM_NAMES + LOCALIZATION_ONLY


def _rueca_config(name: str) -> RuecaConfig:
    return RuecaConfig.from_env(mode=name.removeprefix("rueca-"))


def canonical_arm(name: str) -> str:
    """`rueca` under RUECA_BENCH_RUECA_MODE=hybrid|semantic (or with a
    second-stage knob) is the `rueca-<mode>` arm; every other name is
    itself."""
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
        cfg = _rueca_config(name)
        if cfg.arm_name != name:
            raise ValueError(f"{name} needs a second-stage knob (RUECA_EXPAND, RUECA_CENTRALITY_BETA or RUECA_RERANK=1)")
        return RuecaArm(call_timeout, index_timeout, cfg)
    raise ValueError(f"unknown arm {name}")


def run_label(name: str) -> str:
    """Directory of an arm's cached outputs under `runs/<suite>/`: the arm
    name, plus the sweep setting for the vector arm (`vector/<tag>`) and
    Rueca's vector modes (`rueca-hybrid/<tag>`)."""
    if name == "vector":
        return f"vector/{VectorConfig.from_env().tag()}"
    if name in RUECA_MODE_ARMS:
        cfg = _rueca_config(name)
        # `rueca-lexical` without a second stage is today's `rueca`: no outputs of its own
        return cfg.run_label() if cfg.arm_name == name else f"{name}/none"
    return name
