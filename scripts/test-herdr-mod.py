#!/usr/bin/env python3
"""Exercise the backend and render real fixture previews with Herdr's checker.

Run after cargo build -p easel-mod -p easel-mcp. No real panes or user data
are touched; a private backend and temporary mod are removed on exit.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target/debug/gpui-artifacts-mod"


def check_mcp_route(directory):
    project = directory / "mcp-project"
    project.mkdir()
    fake = directory / "herdr-test"
    log = directory / "split.json"
    pid = directory / "backend.pid"
    fake.write_text(
        f"#!{sys.executable}\n"
        "import json, subprocess, sys\n"
        f"log = {str(log)!r}\n"
        "if sys.argv[1] == 'ls':\n"
        " print(json.dumps({'spaces':[{'tabs':[{'terminals':[{'terminal':'test-terminal','pane':'caller-pane'}]}]}]}))\n"
        "else:\n"
        " assert sys.argv[1:3] == ['do','pane.split'], sys.argv\n"
        " args = json.loads(sys.argv[3])\n"
        " with open(log, 'w') as f: json.dump(args, f)\n"
        " child = subprocess.Popen([args['props']['helper'], 'serve', args['props']['project']], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n"
        f" with open({str(pid)!r}, 'w') as f: f.write(str(child.pid))\n"
        " print(json.dumps({'pane':'easel-pane'}))\n"
    )
    fake.chmod(0o700)
    env = {**os.environ, "GPUI_ARTIFACTS_PROJECT": str(project),
           "GPUI_ARTIFACTS_SOCKET": str(directory / "route.sock"),
           "GPUI_ARTIFACTS_HERDR": str(fake), "HC_TERMINAL_ID": "test-terminal",
           "HERDR_APP_HOME": str(directory / "herdr-home")}
    env.pop("GPUI_ARTIFACTS_VIEWER", None)
    env.pop("GPUI_ARTIFACTS_MOD_HELPER", None)
    try:
        message = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "render_gpui", "arguments": {"id": "routed", "title": "Route test", "source": 'div().child("Through a mod")'}}}
        result = subprocess.run([str(ROOT / "target/debug/gpui-artifacts-mcp")],
                                input=json.dumps(message) + "\n", capture_output=True, text=True, env=env, check=True)
        reply = json.loads(result.stdout)
        assert reply["result"]["isError"] is False, reply
        args = json.loads(log.read_text())
        assert args["pane"] == "caller-pane" and args["mod"] == "easel", args
        assert args["props"]["project"] == str(project), args
        assert (directory / "herdr-home/mods/easel/main.js").read_bytes() == (ROOT / "mods/easel/main.js").read_bytes()
        assert (project / ".gpui-artifacts/state.json").is_file()
        print("MCP opened the mod beside the calling pane with the explicit project.")
    finally:
        if pid.exists():
            try:
                os.kill(int(pid.read_text()), signal.SIGTERM)
            except ProcessLookupError:
                pass


def main():
    with tempfile.TemporaryDirectory(prefix="easel-mod-check-") as temporary:
        directory = Path(temporary)
        check_mcp_route(directory)
        project = directory / "project"
        project.mkdir()
        env = {**os.environ, "GPUI_ARTIFACTS_SOCKET": str(directory / "mcp.sock")}
        server = subprocess.Popen([str(BINARY), "serve", str(project)], env=env)
        try:
            deadline = time.monotonic() + 5
            while not (directory / "mcp.sock").exists():
                if server.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("backend failed to start")
                time.sleep(0.025)

            def request(value):
                with socket.socket(socket.AF_UNIX) as connection:
                    connection.connect(str(directory / "mcp.sock"))
                    connection.sendall((json.dumps(value) + "\n").encode())
                    return json.loads(connection.makefile().readline())

            def action(value):
                result = subprocess.run([str(BINARY), "call", str(project), json.dumps(value)],
                                        env=env, capture_output=True, text=True, check=True)
                return json.loads(result.stdout)

            assert request({"type": "ping"})["type"] == "pong"
            for fixture in sorted((ROOT / "fixtures").glob("*.rs")):
                reply = request({"type": "upsert", "artifact": {
                    "id": fixture.stem, "title": fixture.stem.title(), "source": fixture.read_text(),
                    "mode": "auto", "frame": {"width": None, "height": None}}})
                assert reply["type"] == "artifact", reply
            snapshot = action({"action": "watch", "revision": 0})
            assert len(snapshot["frames"]) == 4
            action({"action": "annotate", "id": "card", "version": 1, "path": [], "note": "More spacing"})
            action({"action": "annotate", "id": "card", "version": 1, "kind": "drawing", "shape": "Arrow", "points": {"from": {"x": 30, "y": 30}, "to": {"x": 80, "y": 80}}})
            action({"action": "annotate", "id": "card", "version": 1, "kind": "note", "note": "Keep this color"})
            action({"action": "send", "text": "Please adjust this"})
            report = request({"type": "chat"})["report"]
            assert report["messages"][0]["attachments"][0]["note"] == "More spacing"
            assert request({"type": "chat"})["report"]["messages"] == []
            assert request({"type": "say", "text": "I will update the spacing."})["type"] == "done"
            snapshot = action({"action": "watch", "revision": 0})
            # Exercise the actual full mod, replacing only its transport startup
            # with a snapshot. A render_error action fails the check immediately.
            mod = directory / "mod"
            mod.mkdir()
            for name in ["mod.json", "main.js", "renderer.js"]:
                shutil.copy(ROOT / "mods/easel" / name, mod / ("view.js" if name == "main.js" else name))
            cases = [(frame, False, "select") for frame in snapshot["frames"]]
            cases += [(snapshot["frames"][0], False, "Pen"), (snapshot["frames"][0], True, "select")]
            for frame, source, tool in cases:
                selected = f"{frame['summary']['id']}:{frame['summary']['version']}"
                (mod / "main.js").write_text(
                    'import Easel from "./view.js";\n'
                    'export default class Check extends Easel {\n'
                    'init(p, cx) { super.init(p, cx);\n'
                    f'this.project = {json.dumps(str(project))}; this.error = null;\n'
                    f'this.snapshot = {json.dumps(snapshot)}; this.selected = {json.dumps(selected)};\n'
                    f'this.showSource = {json.dumps(source)}; this.tool = {json.dumps(tool)}; this.updateSource(); }}\n'
                    'action(r) { throw new Error(JSON.stringify(r)); }\n}\n')
                subprocess.run(["herdr-app", "check", str(mod)], check=True)
                print(f"Rendered {selected}: {frame['summary']['status']['state']}")
            print("Backend protocol, persistence, annotations, chat and fixture previews passed.")
        finally:
            server.terminate()
            server.wait(timeout=5)


if __name__ == "__main__":
    main()
