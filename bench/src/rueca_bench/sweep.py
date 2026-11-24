"""`rueca-bench sweep`: the fusion sweep of RFC 0007 §7.6 on the calibration
split, through the Rueca arm in `hybrid` (or `semantic`) mode.

Every setting is a `RuecaConfig` (α × normalization × N, one preset) with
its own run directory, `runs/s1/rueca-<mode>/<preset>-a<α>-n<N>-<norm>/`,
so settings never collide and an interrupted sweep resumes. Per instance
the repository is indexed **once** with `rueca index --embed=<preset>` (the
store vector cache, shared across instances through `RUECA_VECTOR_DIR`,
makes a later commit re-embed only changed symbols), then only the queries
are re-run for each pending setting: the fusion knobs reach the binary as
`RUECA_FUSION_ALPHA` / `_N` / `_NORM`. The efficiency search is skipped
(tuning reads localization only).

Then the `compare` tables (base `grep` by default) and the plateau choice:

- objective: macro over languages of ½ (File Acc@5 + Function Acc@10),
  `both` = the mean of `kw` and `raw` (the default), or one variant;
- one instance = 1 / n of the objective (n pooled instances: flipping both
  metrics of one instance in a language of average size moves the macro by
  about that much);
- the plateau is a maximal run of consecutive α values of one
  (normalization, N) curve whose objectives are all within one instance of
  the best setting; the widest run wins (then the higher mean objective,
  then the order given), and its centre is chosen (an even run: the better
  of the two middle points, the lower α on a tie), not the arg-max;
- leave one language out: the choice is redone without each language (its
  tolerance 1 / (n − n_l)); the choice is stable on that language when the
  setting chosen without it scores on it within one instance (1 / n_l) of
  the setting chosen with every language.
"""

from __future__ import annotations

import itertools
from dataclasses import dataclass, field
from pathlib import Path

from . import s1
from .arms.rueca import RuecaArm, RuecaConfig
from .common import CACHE, RUNS_DIR, load_manifest, log, read_json, safe_name, write_json
from .compare import POOLED, Comparison, macro_objective, render
from .corpus import worktree
from .metrics import mean

EPS = 1e-9


def parse_alphas(text: str) -> list[float]:
    """`0,0.25,1`, or an arithmetic progression `0,0.1,...,1`."""
    parts = [p.strip() for p in text.split(",") if p.strip()]
    if "..." not in parts:
        return [float(p) for p in parts]
    if len(parts) != 4 or parts[2] != "...":
        raise ValueError(f"alpha progression {text!r}: write first,second,...,last")
    first, second, last = float(parts[0]), float(parts[1]), float(parts[3])
    step = second - first
    if step <= 0 or last < first:
        raise ValueError(f"alpha progression {text!r} must increase")
    count = int(round((last - first) / step)) + 1
    return [round(first + i * step, 10) for i in range(count)]


def output_path(cfg: RuecaConfig, iid: str) -> Path:
    return RUNS_DIR / "s1" / cfg.run_label() / f"{safe_name(iid)}.json"


def _done(cfg: RuecaConfig, iid: str) -> bool:
    p = output_path(cfg, iid)
    return p.exists() and read_json(p).get("status") in ("ok", "index_failed")


@dataclass
class Sweep:
    mode: str = "hybrid"
    preset: str | None = None
    alphas: list[float] = field(default_factory=lambda: parse_alphas("0,0.1,...,1"))
    norms: list[str] = field(default_factory=lambda: ["minmax", "theoretical"])
    ns: list[int] = field(default_factory=lambda: [100])
    split: str = "calibration"

    def configs(self) -> list[RuecaConfig]:
        """Every setting (α = 0 only in `semantic` mode, which ignores α)."""
        base = RuecaConfig.from_env(mode=self.mode)
        preset = self.preset or base.preset
        alphas = [0.0] if self.mode == "semantic" else self.alphas
        return [RuecaConfig(self.mode, preset, a, n, norm) for norm, n, a in itertools.product(self.norms, self.ns, alphas)]

    def columns(self) -> list[str]:
        return [f"{c.arm_name}:{c.tag()}" for c in self.configs()]

    # -- running ----------------------------------------------------------------------

    def run(self, only: list[str] | None = None, languages: set[str] | None = None, workers: int = 1, force: bool = False, prune: bool = False, arm: RuecaArm | None = None) -> None:
        configs = self.configs()
        arm = arm or RuecaArm(config=configs[0])
        if not arm.available()[0]:
            raise SystemExit(f"rueca-bench sweep: {arm.available()[1]}")
        try:
            arm.preflight()
        except RuntimeError as exc:
            raise SystemExit(f"rueca-bench sweep: {exc}") from exc
        arm.version()  # once: shared by every setting's records
        insts = s1.select_instances(only, languages, self.split)
        limit = load_manifest()["s1"].get("result_limit", 100)
        log(f"sweep: {len(insts)} instances x {len(configs)} settings ({configs[0].arm_name})")
        s1.by_repo(insts, lambda inst: sweep_instance(arm, inst, configs, limit, force), "sweep", workers, prune)

    # -- reading ----------------------------------------------------------------------

    def compare(self, base: str = "grep", cache: Path | None = None) -> dict:
        return Comparison(cache or CACHE, self.split, base, self.columns()).run()


def sweep_instance(arm: RuecaArm, inst: dict, configs: list[RuecaConfig], limit: int, force: bool) -> None:
    """Index one instance once, then answer its queries under every pending
    setting."""
    pending = [c for c in configs if force or not _done(c, inst["id"])]
    if not pending:
        return
    s1.s1_gold(inst)
    qs = s1.queries_for(inst)
    with worktree(inst["repo"], inst["base_commit"], f"s1-{inst['id']}") as root:
        first = arm.with_config(pending[0])
        log(f"sweep index {inst['id']} ({len(pending)} settings)")
        rec0, h = s1.index_record(first, inst, root)
        try:
            for cfg in pending:
                one = arm.with_config(cfg)
                rec = {**rec0, "arm": one.name, "setting": cfg.as_dict()}
                if h.ok:
                    s1.query_record(one, h, rec, qs, limit, eff=False)
                rec["arm_version"] = one.version()
                write_json(output_path(cfg, inst["id"]), rec)
        finally:
            first.cleanup(h)


# --- plateau choice (RFC 0007 §7.6) ----------------------------------------------------


def objective(res: dict, label: str, variant: str, skip: str | None = None) -> float | None:
    table = res["columns"][label]["table"]
    order = res["groups"]
    if variant == "both":
        return mean([macro_objective(table, v, order, skip) for v in ("kw", "raw")])
    return macro_objective(table, variant, order, skip)


def language_objective(res: dict, label: str, variant: str, group: str) -> float | None:
    table = res["columns"][label]["table"]
    variants = ("kw", "raw") if variant == "both" else (variant,)
    return mean([(table[v].get(group) or {}).get("objective") for v in variants])


def _runs(points: list[tuple[float, float | None]], floor: float) -> list[list[tuple[float, float]]]:
    """Maximal runs of consecutive points whose value is at least `floor`."""
    out: list[list[tuple[float, float]]] = []
    cur: list[tuple[float, float]] = []
    for a, v in points:
        if v is not None and v >= floor - EPS:
            cur.append((a, v))
            continue
        if cur:
            out.append(cur)
        cur = []
    if cur:
        out.append(cur)
    return out


def _centre(run: list[tuple[float, float]]) -> tuple[float, float]:
    k = len(run)
    if k % 2:
        return run[k // 2]
    left, right = run[k // 2 - 1], run[k // 2]
    return right if right[1] > left[1] + EPS else left


def curves(configs: list[RuecaConfig]) -> dict[tuple[str, int], list[RuecaConfig]]:
    """Settings by (normalization, N), sorted by α."""
    out: dict[tuple[str, int], list[RuecaConfig]] = {}
    for c in configs:
        out.setdefault((c.norm, c.fusion_n), []).append(c)
    return {k: sorted(v, key=lambda c: c.alpha) for k, v in out.items()}


def _label(c: RuecaConfig) -> str:
    return f"{c.arm_name}:{c.tag()}"


def choose(res: dict, configs: list[RuecaConfig], variant: str, n: int, skip: str | None = None) -> dict | None:
    """The plateau centre (module docstring); `skip` leaves a language out."""
    values = {_label(c): objective(res, _label(c), variant, skip) for c in configs}
    known = {k: v for k, v in values.items() if v is not None}
    if not known or n <= 0:
        return None
    best_label = max(known, key=lambda k: known[k])
    tol = 1.0 / n
    candidates = []
    for idx, (key, cs) in enumerate(curves(configs).items()):
        for run in _runs([(c.alpha, values[_label(c)]) for c in cs], known[best_label] - tol):
            candidates.append((len(run), mean(v for _, v in run), -idx, key, run))
    width, _, _, (norm, fusion_n), run = max(candidates, key=lambda t: t[:3])
    alpha, value = _centre(run)
    chosen = next(c for c in configs if (c.norm, c.fusion_n, c.alpha) == (norm, fusion_n, alpha))
    return {
        "best": {"setting": best_label, "objective": known[best_label]},
        "tolerance": tol,
        "plateau": {"norm": norm, "n": fusion_n, "alphas": [a for a, _ in run]},
        "setting": _label(chosen),
        "alpha": alpha,
        "norm": norm,
        "fusion_n": fusion_n,
        "objective": value,
    }


def _pooled_n(res: dict, labels: list[str]) -> int:
    return max((res["columns"][k]["table"]["kw"][POOLED]["n"] for k in labels if k in res["columns"]), default=0)


def _group_n(res: dict, labels: list[str], group: str) -> int:
    return max(((res["columns"][k]["table"]["kw"].get(group) or {}).get("n", 0) for k in labels if k in res["columns"]), default=0)


def leave_one_out(res: dict, configs: list[RuecaConfig], variant: str, full: dict) -> list[dict]:
    labels = [_label(c) for c in configs]
    n = _pooled_n(res, labels)
    out = []
    for g in res["groups"]:
        n_g = _group_n(res, labels, g)
        alt = choose(res, configs, variant, n - n_g, skip=g) if n_g else None
        if alt is None:
            continue
        on_alt = language_objective(res, alt["setting"], variant, g)
        on_full = language_objective(res, full["setting"], variant, g)
        ok = None if on_alt is None or on_full is None else on_alt >= on_full - 1.0 / n_g - EPS
        out.append({"group": g, "n": n_g, "setting": alt["setting"], "alpha": alt["alpha"], "norm": alt["norm"], "on_group": on_alt, "full_on_group": on_full, "stable": ok})
    return out


def plateau(res: dict, configs: list[RuecaConfig], variant: str = "both") -> dict:
    """The §7.6 choice and its leave-one-language-out check."""
    labels = [_label(c) for c in configs]
    n = _pooled_n(res, labels)
    full = choose(res, configs, variant, n)
    out: dict = {"variant": variant, "n": n, "choice": full, "leave_one_out": []}
    if full is not None:
        out["leave_one_out"] = leave_one_out(res, configs, variant, full)
    out["curves"] = {f"{norm} N{fn}": [[c.alpha, objective(res, _label(c), variant)] for c in cs] for (norm, fn), cs in curves(configs).items()}
    return out


def _pct(x: float | None) -> str:
    return "—" if x is None else f"{100 * x:.1f}"


def _curve_table(p: dict) -> list[str]:
    names = list(p["curves"])
    alphas = sorted({a for pts in p["curves"].values() for a, _ in pts})
    by = {k: dict((a, v) for a, v in pts) for k, pts in p["curves"].items()}
    L = ["| α | " + " | ".join(names) + " |", "|---:|" + "---:|" * len(names)]
    for a in alphas:
        L.append(f"| {a:g} | " + " | ".join(_pct(by[k].get(a)) for k in names) + " |")
    return L


def _lolo_table(p: dict) -> list[str]:
    L = ["| Held out | n | Choice without it | Its objective there | Full choice there | Within one instance |", "|---|---:|---|---:|---:|---|"]
    for r in p["leave_one_out"]:
        st = "—" if r["stable"] is None else ("yes" if r["stable"] else "**no**")
        L.append(f"| {r['group']} | {r['n']} | α {r['alpha']:g}, {r['norm']} | {_pct(r['on_group'])} | {_pct(r['full_on_group'])} | {st} |")
    return L


def render_plateau(p: dict) -> str:
    L = ["## Plateau choice (RFC 0007 §7.6)", ""]
    c = p["choice"]
    if c is None:
        return "\n".join([*L, "No setting has results yet.", ""]) + "\n"
    L += [
        f"Objective: macro over languages of ½ (File Acc@5 + Fn Acc@10), variant `{p['variant']}`; one instance = 1/{p['n']} = {100 * c['tolerance']:.2f} points.",
        "",
        *_curve_table(p),
        "",
        f"- Best setting: `{c['best']['setting']}` ({_pct(c['best']['objective'])}).",
        f"- Plateau within one instance of it: {c['norm']}, N {c['fusion_n']}, α ∈ {{{', '.join(f'{a:g}' for a in c['plateau']['alphas'])}}}.",
        f"- **Choice: α = {c['alpha']:g}, {c['norm']}, N {c['fusion_n']}** (`{c['setting']}`, {_pct(c['objective'])}).",
        "",
        "### Leave one language out",
        "",
        *_lolo_table(p),
        "",
    ]
    return "\n".join(L) + "\n"


def report(sweep: Sweep, base: str = "grep", variant: str = "both", cache: Path | None = None) -> tuple[str, dict]:
    """Markdown (compare tables, then the plateau) and the JSON result."""
    res = sweep.compare(base, cache)
    p = plateau(res, sweep.configs(), variant)
    return render(res, variant) + "\n" + render_plateau(p), {"compare": res, "plateau": p}
