"""Package an already-built Windows executable with the small preview installer.

Native release builds should use build-windows.ps1 and Tauri's full NSIS pipeline.
This helper also runs on Linux when makensis and its NSIS data are available.
"""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--makensis", default="makensis")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
version = json.loads((root / "apps/desktop/tauri.conf.json").read_text(encoding="utf-8"))["version"]
if not re.fullmatch(r"\d+\.\d+\.\d+", version):
    raise SystemExit("The preview installer requires a numeric major.minor.patch version.")
package_version = json.loads((root / "package.json").read_text(encoding="utf-8"))["version"]
if package_version != version:
    raise SystemExit("Tauri and package.json versions differ; update both before packaging.")
binary = args.binary.resolve(strict=True)
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=True)
portable = output / "easy-mouse-connect.exe"
if binary != portable:
    shutil.copy2(binary, portable)
shutil.copy2(root / "packaging/START-HERE.txt", output / "START-HERE.txt")
subprocess.run([
    args.makensis, "-WX", "-V2",
    f"-DAPP_VERSION={version}",
    f"-DAPP_BINARY={portable}",
    f"-DSETUP_OUT={output / f'EasyMouseConnect-{version}-preview-setup.exe'}",
    f"-DTEST_README={root / 'packaging/START-HERE.txt'}",
    f"-DAPP_ICON={root / 'apps/desktop/icons/icon.ico'}",
    str(root / "packaging/windows-preview.nsi"),
], check=True, cwd=root)
print(f"Preview package: {output}")
