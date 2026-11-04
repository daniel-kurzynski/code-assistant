"""A minimal stdio MCP server with a fixed list of tools, for the Tools menu.

    python3 fake_mcp.py jira
"""

import json
import sys

TOOLS = {
    "jira": ["search_issues", "get_issue", "create_issue", "update_issue", "add_comment",
             "transition_issue", "list_projects", "list_sprints", "get_sprint",
             "assign_issue", "link_issues", "get_transitions"],
}


def main():
    name = sys.argv[1]
    tools = [{"name": t, "description": t.replace("_", " ").capitalize(),
              "inputSchema": {"type": "object", "properties": {}},
              "annotations": {"readOnlyHint": t.startswith(("get", "list", "search"))}}
             for t in TOOLS[name]]
    for line in sys.stdin:
        msg = json.loads(line)
        if "id" not in msg:
            continue
        method = msg.get("method")
        if method == "initialize":
            result = {"protocolVersion": msg["params"].get("protocolVersion", "2025-03-26"),
                      "capabilities": {"tools": {}}, "serverInfo": {"name": name, "version": "1.0"}}
        elif method == "tools/list":
            result = {"tools": tools}
        elif method == "tools/call":
            result = {"content": [{"type": "text", "text": "ok"}]}
        elif method == "ping":
            result = {}
        else:
            reply = {"jsonrpc": "2.0", "id": msg["id"],
                     "error": {"code": -32601, "message": f"unknown method {method}"}}
            print(json.dumps(reply), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)


if __name__ == "__main__":
    main()
