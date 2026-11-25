# Fluency probe (RFC 0009 phase 3)

Can a model that has never seen the repositories write a correct code-mode
script from the API reference alone? Results and their reading:
`docs/bench/multistep-results.md` (*Fluency probe*) and RFC 0009 §14.

## Protocol

- **Tasks**: 10 of the multi-step suite's 25 — ms01, ms02, ms07, ms09,
  ms14, ms15, ms17, ms19, ms22, ms25 — chosen among the 18 tasks whose
  reference answers match gold, spanning kinds (2-hop callers, callers ∩
  callers, top-N by fan-in, exactly-2-hop count, callees ∩ callees, the
  callee with most callers, cross-repository callers and classes, routes
  called by one client only, a ref diff).
- **Languages**: JavaScript and Starlark, so 20 cells.
- **Writer**: one fresh Claude Opus subagent per (task, language), given
  only the prompt of `prompt-template.md`: a short preamble, the task's
  statement (`rueca-bench multistep tasks`) and the `describe_api` output
  for that language (`rueca-bench multistep describe-api --language js`
  or `starlark`). No repository access, no reference script, no way to run
  anything. It wrote one script.
- **Scoring**: the script is run unchanged with `rueca-bench multistep
  probe --task <id> --language <lang> --script <file>` (`execute` with its
  defaults against the task's corpus) and its answer compared exactly with
  gold. On failure, exactly one repair attempt: a fresh subagent given the
  same prompt, its script and the `execute` output (the error line, or the
  returned answer when there was no error).
- **Limits of the method**: n = 1 per cell and one model. An indication,
  not a measurement of model fluency in general.
- **API version**: the scripts were written against the API before the
  four friction fixes of RFC 0009 §14.6 (`local`, `repo` / `ref` on every
  symbol, per-symbol `changed`, tests nested in test blocks); they are kept
  as written, so re-scoring them today may not reproduce `out/`.

The `describe_api` texts the writers read had SHA-256
`ef64f53a2b462fe975d41520b5e27975456f2b3e92d9820b8e3db956e1cd78fc` (`js`,
15,444 bytes) and
`0cb321afcaf74f1208229a78597678ea4903ef8302826b6d4a90a0587c9d772b`
(`starlark`, 15,176 bytes); a binary built before the fixes of RFC 0009
§14.6 (e.g. at the commit that added this directory) regenerates them.
Today's `describe_api` is longer (4,260 / 4,338 tokens with the new fields).

## Files

- `prompt-template.md`: the prompt, with `{LANGUAGE}` (`JavaScript` /
  `Starlark`), `{STATEMENT}`, `{LANG}` (`js` / `starlark`) and
  `{DESCRIBE_API}` to fill in. All 20 prompts were exactly this template.
- `scripts/<task>-<lang>.js|.star`: the 20 first-try scripts, unchanged.
  `scripts/ms19-js.try2.js`: the one repair that ran.
- `out/<task>-<lang>.try<N>.json`: the probe runner's output line for each
  run (`correct`, `error`, `result_preview`, `script_tokens`,
  `result_tokens`, `calls` = engine calls, `truncated`).

The Starlark repair of ms19 was not obtained: the repair subagent's file
write was blocked by the harness's permission system, a tooling failure
unrelated to the language.

To re-score a script:

```sh
uv run --project bench rueca-bench multistep probe --task ms19 --language js \
  --script bench/multistep/probe/scripts/ms19-js.js
```
