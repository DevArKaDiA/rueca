//! Service mode keeps code mode (RFC 0009 §7.3) off even when the
//! environment asks for it: `tools/list` has the eight tools and `execute`
//! is an unknown tool. The only test of this binary, so it may set the
//! variable before anything else runs.

mod common;

use common::{Server, TOKEN, populated};
use serde_json::json;

#[test]
fn code_mode_stays_off_in_service_mode() {
    // SAFETY: no other thread exists yet in this test binary's only test.
    unsafe { std::env::set_var(rueca_mcp::codemode::ENV_ENABLE, "1") };
    assert!(rueca_mcp::codemode::enabled_by_env());

    let _p = populated("memory://serve-code-mode");
    let s = Server::start("memory://serve-code-mode", "");
    s.wait_ready();
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let tools = s.mcp(Some(TOKEN), &list, &[("mcp-protocol-version", "2025-06-18")]);
    assert_eq!(tools.status, 200, "{}", tools.body);
    let listed = tools.json()["result"]["tools"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = listed.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names, rueca_query::TOOL_NAMES);

    for tool in ["execute", "describe_api"] {
        let r = s.call(TOKEN, tool, json!({"code": "return 1"}));
        assert!(r.body.contains(&format!("unknown tool `{tool}`")), "{tool}: {}", r.body);
    }
}
