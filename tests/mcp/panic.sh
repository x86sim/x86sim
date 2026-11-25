#!/bin/bash
# x86Sim: the MCP server survives a tool call that panics (bin/mcp.rs catch_unwind): the call answers isError and the
# next request is still served. Usage: tests/mcp/panic.sh   (needs tools/launcher built)
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
out=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"__test_panic","arguments":{}}}' \
	'{"jsonrpc":"2.0","id":2,"method":"ping"}' | X86SIM_MCP_TEST_PANIC=1 "$X86SIM_ROOT/tools/launcher/target/release/x86sim-mcp" 2>/dev/null)
grep -q '"id":1.*"isError":true' <<<"$out" || grep -q '"isError":true.*"id":1' <<<"$out" || { echo "FAIL: no error answer for the panicking call: $out"; exit 1; }
grep -q '"id":2' <<<"$out" || { echo "FAIL: the server ended after the panic: $out"; exit 1; }
echo "ok   a panicking tool call is an error; the server goes on"
