//! End to end for Markdown: `rueca index` on a working tree with documents
//! and code, then sections and links through `rueca query`.

mod common;

use serde_json::json;

use common::*;

const README: &str = "---
title: Demo
---

<p align=\"center\"><img src=\"assets/logo.svg\" alt=\"Logo\"></p>

# Demo

[![CI](https://example.com/ci.svg)](https://example.com/ci)

Demo turns invoices into ledgers.

## Install

Read the [guide](docs/guide.md#install) first.

### Linux

```sh
cargo install demo
```

### macOS

Use [Homebrew](https://brew.sh).

## Usage

Entry point: [main](src/main.py). See [Install](#install).

## Usage

The second usage section.
";

const GUIDE: &str = "# Guide

## Install

Back to the [readme](../README.md); the [API](api.md) and the [docs index](./).
";

#[test]
fn markdown_sections_and_links() {
    let p = python_project();
    write(&p.root, "README.md", README);
    write(&p.root, "docs/guide.md", GUIDE);
    write(&p.root, "docs/api.md", "# API\n\nThe API.\n");
    write(&p.root, "docs/README.md", "# Docs\n");
    write(&p.root, "src/main.py", "def main():\n    return 1\n");
    write(&p.root, "assets/logo.svg", "<svg/>\n");
    let report = index(&p);
    assert_eq!(report["files_unsupported"], 1, "only the svg has no pack: {report}");

    // search_symbols finds sections by heading words, Section is a default label.
    let r = query(&p, "search_symbols", json!({"query": "install linux"}));
    let top = &r["results"][0];
    assert_eq!(top["qualified_name"], "README.md:Demo > Install > Linux", "{r}");
    assert_eq!(top["label"], "Section");
    assert_eq!(top["id"], "local/proj@worktree:README.md#Demo > Install > Linux");
    assert_eq!(top["signature"], "### Linux");
    assert_eq!((top["start_line"].as_u64(), top["end_line"].as_u64()), (Some(17), Some(21)), "{r}");
    let r = query(&p, "search_symbols", json!({"query": "install", "language": "markdown", "labels": ["Section"]}));
    let hits = qns(&r["results"]);
    assert!(hits.contains(&"docs/guide.md:Guide > Install".to_string()), "{r}");
    assert!(r["results"].as_array().unwrap().iter().all(|h| h["label"] == "Section"));

    // get_symbol: parent / children follow the heading tree; doc = first paragraph.
    let r = query(&p, "get_symbol", json!({"qualified_name": "Demo > Install"}));
    assert_eq!(r["symbol"]["language"], "markdown");
    assert_eq!(r["symbol"]["doc"], "Read the guide first.");
    assert_eq!(r["parent"]["qualified_name"], "README.md:Demo");
    assert_eq!(qns(&r["children"]), ["README.md:Demo > Install > Linux", "README.md:Demo > Install > macOS"]);
    let r = query(&p, "get_symbol", json!({"qualified_name": "README.md:Demo"}));
    assert_eq!(r["symbol"]["doc"], "Demo turns invoices into ledgers.");
    // By id round trip, and by name + path.
    let id = "local/proj@worktree:README.md#Demo > Usage (2)";
    let r = query(&p, "get_symbol", json!({"id": id}));
    assert_eq!(r["symbol"]["name"], "Usage");
    assert_eq!(r["symbol"]["id"], id);
    let r = query(&p, "get_symbol", json!({"name": "Install", "path": "docs/guide.md"}));
    assert_eq!(r["symbol"]["qualified_name"], "docs/guide.md:Guide > Install");

    // get_file_outline: the heading tree.
    let r = query(&p, "get_file_outline", json!({"path": "README.md"}));
    assert_eq!(r["language"], "markdown");
    let syms = r["symbols"].as_array().unwrap();
    assert_eq!(qns(&r["symbols"]), ["README.md:Demo"], "{r}");
    let demo_children = qns(&syms[0]["children"]);
    assert_eq!(
        demo_children,
        ["README.md:Demo > Install", "README.md:Demo > Usage", "README.md:Demo > Usage (2)"],
        "{r}"
    );
    assert_eq!(
        qns(&syms[0]["children"][0]["children"]),
        ["README.md:Demo > Install > Linux", "README.md:Demo > Install > macOS"]
    );

    // get_snippet by qualified name returns the whole section, subsections included.
    let r = query(&p, "get_snippet", json!({"qualified_name": "README.md:Demo > Install"}));
    let text = r["text"].as_str().unwrap();
    assert!(text.starts_with("## Install\n"), "{r}");
    assert!(text.contains("cargo install demo") && text.ends_with("Use [Homebrew](https://brew.sh)."), "{r}");
    assert!(!text.contains("## Usage"));

    // search_code places matches inside their section.
    let r = query(&p, "search_code", json!({"pattern": "cargo install", "path_glob": "*.md"}));
    assert_eq!(r["matches"][0]["symbol"]["qualified_name"], "README.md:Demo > Install > Linux", "{r}");

    // trace IMPORTS from a document: linked files, with the link as call site.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "README.md", "edge_types": ["IMPORTS"], "direction": "callees", "depth": 2}),
    );
    let mut linked: Vec<(String, u64)> = r["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["qualified_name"].as_str().unwrap().to_string(), n["depth"].as_u64().unwrap()))
        .collect();
    linked.sort();
    let expect: Vec<(String, u64)> =
        [("assets/logo.svg", 1), ("docs/README.md", 2), ("docs/api.md", 2), ("docs/guide.md", 1), ("src/main.py", 1)]
            .iter()
            .map(|(q, d)| (q.to_string(), *d))
            .collect();
    assert_eq!(linked, expect, "{r}");
    let guide_edge = r["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["dst"] == "local/proj@worktree:docs/guide.md")
        .unwrap_or_else(|| panic!("{r}"));
    assert_eq!(guide_edge["kind"], "IMPORTS");
    assert_eq!(guide_edge["call_site"], json!({"path": "README.md", "line": 15}));

    // Reverse: which documents link to the code file.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "src/main.py", "edge_types": ["IMPORTS"], "direction": "callers", "depth": 1}),
    );
    assert_eq!(qns(&r["nodes"]), ["README.md"], "{r}");

    // Sections never become call targets of code.
    let r = query(&p, "trace", json!({"qualified_name": "README.md:Demo > Usage"}));
    assert!(r["nodes"].as_array().unwrap().is_empty(), "{r}");
}
