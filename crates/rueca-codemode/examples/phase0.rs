//! RFC 0009 phase 0 measurements: start-up, per-call overhead and limit
//! behaviour of every engine this build has. Run it in release:
//!
//! ```sh
//! cargo run --release -p rueca-codemode --example phase0
//! ```
//!
//! Limit probes run in a child process each (`phase0 probe <name>`), so a
//! probe's peak memory is its own and a probe that never stops is killed
//! by the parent after [`PROBE_KILL`].

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rueca_codemode::{Host, Language, Limits, Sandbox, sandbox};
use serde_json::{Value, json};

const TOOLS: &[&str] = &["search_symbols", "get_symbol"];
const COLD_RUNS: usize = 500;
const CALL_RUNS: usize = 30;
const CALLS: u32 = 1000;
const PROBE_KILL: Duration = Duration::from_secs(20);

/// Answers every call with the same small object.
struct NoopHost;

impl Host for NoopHost {
    fn call(&mut self, _tool: &str, _args: Value) -> Result<Value, String> {
        Ok(json!({"id": "r@0123456789ab:src/a.py#f", "name": "f", "kind": "Function", "line": 12}))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("probe") {
        probe(args.get(2).map(String::as_str).unwrap_or_default());
        return;
    }
    for &language in Language::available() {
        construction(language);
        cold_start(language);
        per_call(language);
    }
    for name in PROBES.iter().map(|p| p.name) {
        run_probe(name);
    }
}

fn engine(language: Language) -> Box<dyn Sandbox + Send + Sync> {
    sandbox(language, TOOLS).expect("language built in")
}

/// Median and 95th percentile, in microseconds.
fn stats(mut samples: Vec<Duration>) -> (f64, f64) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() as f64 - 1.0) * q).round() as usize].as_secs_f64() * 1e6;
    (at(0.5), at(0.95))
}

fn time_runs(sb: &dyn Sandbox, script: &str, limits: &Limits, runs: usize) -> Vec<Duration> {
    (0..runs)
        .map(|_| {
            let started = Instant::now();
            sb.run(script, &mut NoopHost, limits).expect("script runs");
            started.elapsed()
        })
        .collect()
}

fn construction(language: Language) {
    let samples: Vec<Duration> = (0..50)
        .map(|_| {
            let started = Instant::now();
            drop(engine(language));
            started.elapsed()
        })
        .collect();
    let (p50, p95) = stats(samples);
    println!("{language}\tsandbox construction (once per server)\tp50 {p50:.1} us\tp95 {p95:.1} us");
}

fn cold_start(language: Language) {
    let sb = engine(language);
    let script = pick(language, "return 1;", "result = 1");
    let (p50, p95) = stats(time_runs(sb.as_ref(), script, &Limits::default(), COLD_RUNS));
    println!("{language}\tcold start, {COLD_RUNS} runs\tp50 {p50:.1} us\tp95 {p95:.1} us");
}

fn per_call(language: Language) {
    let sb = engine(language);
    let limits = Limits { max_calls: CALLS + 1, ..Limits::default() };
    let calls = pick(
        language,
        "let n = 0; for (let i = 0; i < 1000; i++) { const r = await rueca.getSymbol({id: \"x\", i}); n += r.line; } return n;",
        "n = 0\nfor i in range(1000):\n    r = rueca.get_symbol(id = \"x\", i = i)\n    n += r[\"line\"]\nresult = n",
    );
    let empty = pick(
        language,
        "let n = 0; for (let i = 0; i < 1000; i++) { const r = {id: \"x\", i, line: 12}; n += r.line; } return n;",
        "n = 0\nfor i in range(1000):\n    r = {\"id\": \"x\", \"i\": i, \"line\": 12}\n    n += r[\"line\"]\nresult = n",
    );
    let (with_calls, _) = stats(time_runs(sb.as_ref(), calls, &limits, CALL_RUNS));
    let (without, _) = stats(time_runs(sb.as_ref(), empty, &limits, CALL_RUNS));
    let per = (with_calls - without) / f64::from(CALLS);
    println!(
        "{language}\tper host call ({CALLS} calls, no-op host)\t{per:.2} us/call\t(run {with_calls:.0} us, same loop without calls {without:.0} us)"
    );
}

fn pick<'a>(language: Language, js: &'a str, starlark: &'a str) -> &'a str {
    match language {
        Language::Js => js,
        Language::Starlark => starlark,
    }
}

/// One limit probe: a script, its engine and the limits it runs under.
struct Probe {
    name: &'static str,
    language: Language,
    script: &'static str,
    memory_mib: usize,
}

const PROBES: &[Probe] = &[
    Probe { name: "js-loop", language: Language::Js, script: "while (true) {}", memory_mib: 64 },
    Probe {
        name: "js-native-loop",
        language: Language::Js,
        script: "return Array.prototype.indexOf.call({length: 2 ** 40}, 1);",
        memory_mib: 64,
    },
    Probe {
        name: "js-native-includes",
        language: Language::Js,
        script: "return Array.prototype.includes.call({length: 2 ** 40}, 1);",
        memory_mib: 64,
    },
    Probe {
        name: "js-native-copywithin",
        language: Language::Js,
        script: "return Array.prototype.copyWithin.call({length: 2 ** 40}, 0, 1).length;",
        memory_mib: 64,
    },
    Probe {
        name: "js-native-copywithin-sparse",
        language: Language::Js,
        script: "const a = []; a.length = (1 << 22) - 1; a.copyWithin(0, 1); return a.length;",
        memory_mib: 64,
    },
    Probe {
        name: "js-regex-backtracking",
        language: Language::Js,
        script: "return /(a+)+b/.test(\"a\".repeat(40));",
        memory_mib: 64,
    },
    Probe {
        name: "js-native-sort",
        language: Language::Js,
        script: "const a = Array.from({length: 1000000}, (_, i) => -i); a.sort((x, y) => x - y); return a[0];",
        memory_mib: 64,
    },
    Probe {
        name: "js-grow",
        language: Language::Js,
        script: "const a = []; while (true) a.push(\"x\".repeat(10000) + a.length);",
        memory_mib: 64,
    },
    Probe {
        name: "js-one-op-512mib",
        language: Language::Js,
        script: "return \"x\".repeat(1 << 29).length;",
        memory_mib: 64,
    },
    Probe {
        name: "js-catch-oom",
        language: Language::Js,
        script: "let n = 0; while (true) { try { \"x\".repeat(1 << 30); } catch (e) { n++; } }",
        memory_mib: 64,
    },
    Probe {
        name: "star-loop",
        language: Language::Starlark,
        script: "for x in range(2147483647):\n    pass",
        memory_mib: 64,
    },
    Probe {
        name: "star-grow",
        language: Language::Starlark,
        script: "a = []\nfor i in range(2147483647):\n    a.append(\"x\" * 10000 + str(i))",
        memory_mib: 64,
    },
    Probe {
        name: "star-one-op-1gib",
        language: Language::Starlark,
        script: "s = \"x\" * (1 << 30)\nresult = len(s)",
        memory_mib: 64,
    },
    Probe {
        // 256 TiB in one repetition: more than the address space, so the
        // allocation fails at once instead of touching memory.
        name: "star-one-op-256tib",
        language: Language::Starlark,
        script: "s = (\"x\" * 131072) * 2147483647\nresult = len(s)",
        memory_mib: 64,
    },
    Probe {
        name: "star-dict",
        language: Language::Starlark,
        script: "d = {}\nfor i in range(2147483647):\n    d[i] = i",
        memory_mib: 64,
    },
];

/// Child side: runs one probe and prints what happened.
fn probe(name: &str) {
    let Some(p) = PROBES.iter().find(|p| p.name == name) else {
        eprintln!("unknown probe {name}");
        return;
    };
    if !Language::available().contains(&p.language) {
        println!("skipped (engine not built)");
        return;
    }
    let limits = Limits { memory_bytes: p.memory_mib << 20, ..Limits::default() };
    let started = Instant::now();
    let out = engine(p.language).run(p.script, &mut NoopHost, &limits);
    let took = started.elapsed();
    let what = match out {
        Ok(o) => format!("returned {}", o.result),
        Err(e) => format!("{}: {}", e.kind, e.message.lines().next().unwrap_or_default()),
    };
    let ms = took.as_secs_f64() * 1e3;
    println!(
        "{what}\tafter {ms:.1} ms (wall limit {:?}, memory limit {} MiB)\tpeak RSS {}",
        limits.wall_time,
        p.memory_mib,
        peak_rss()
    );
}

/// The process's peak resident set size, as `ps` cannot see it after exit.
fn peak_rss() -> String {
    // SAFETY: getrusage writes into the zeroed struct we pass and nothing else.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid, writable rusage for the call's duration.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return "unknown".into();
    }
    // Bytes on macOS, KiB on Linux.
    let bytes = if cfg!(target_os = "macos") { usage.ru_maxrss } else { usage.ru_maxrss * 1024 };
    format!("{} MiB", bytes >> 20)
}

/// Parent side: runs a probe in a child, killing it after [`PROBE_KILL`].
fn run_probe(name: &str) {
    let exe = std::env::current_exe().expect("own path");
    let mut child = Command::new(exe)
        .args(["probe", name])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn probe");
    let started = Instant::now();
    while child.try_wait().expect("wait").is_none() {
        if started.elapsed() > PROBE_KILL {
            let _ = child.kill();
            let _ = child.wait();
            println!("probe {name}\tnot stopped after {PROBE_KILL:?}; killed");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("probe output");
    let text = String::from_utf8_lossy(&out.stdout);
    let status = if out.status.success() { String::new() } else { format!(" [exit: {}]", out.status) };
    println!("probe {name}\t{}{status}", text.trim());
}
