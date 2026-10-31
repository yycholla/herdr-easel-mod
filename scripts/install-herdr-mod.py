#!/usr/bin/env python3
"""Install this checkout's Easel mod and point the CLI tools at its binaries."""
import datetime
import os
from pathlib import Path
import shutil
import subprocess

root = Path(__file__).resolve().parent.parent
subprocess.run(["cargo", "build", "-p", "easel-mcp", "-p", "easel-mod"], cwd=root, check=True)
config = Path(os.environ.get("HERDR_APP_HOME", str(Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "herdr-app")))
destination = config / "mods/easel"
destination.mkdir(parents=True, exist_ok=True)
stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
names = ["mod.json", "main.js", "renderer.js"]
for name in names:
    source = root / "mods/easel" / name
    target = destination / name
    if target.exists() and target.read_bytes() != source.read_bytes():
        raise SystemExit(f"Refusing to replace existing mod file: {target}. Rename the mod and its MCP host name before installing; see RECIPE.md.")
for name in names:
    shutil.copy2(root / "mods/easel" / name, destination / name)
binary_dir = Path(os.environ.get("EASEL_BIN_DIR", str(Path.home() / ".local/bin")))
binary_dir.mkdir(parents=True, exist_ok=True)
for name in ["gpui-artifacts-mcp", "gpui-artifacts-mod"]:
    target = binary_dir / name
    source = root / "target/debug" / name
    if target.is_symlink() and target.resolve() == source:
        continue
    if target.exists() or target.is_symlink():
        target.rename(target.with_name(f"{name}.{stamp}.bak"))
    target.symlink_to(source)
subprocess.run([os.environ.get("GPUI_ARTIFACTS_HERDR", "herdr-app"), "check", str(destination)], check=True)
print(f"Installed Easel in {destination}")
print(f"CLI tools in {binary_dir}; restart an existing MCP session to load the new routing.")
