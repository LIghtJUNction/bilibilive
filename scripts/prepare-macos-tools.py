#!/usr/bin/env python3
"""Reuse native runner tools and install the fixed official Meson wheel privately."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import urllib.request

MESON_VERSION="1.12.1"
MESON_WHEEL="meson-1.12.1-py3-none-any.whl"
MESON_URL="https://files.pythonhosted.org/packages/57/99/36cd25f9598eef70db97577440a70a360af505f6f01c2338a18d1556079c/"+MESON_WHEEL
MESON_SHA256="930bc7542cbd9f57009e182fd014eba48cf1a0180a7b9006d2d32d8f168d8b02"
TOOLS={"cmake":("cmake","--version",(3,28)),
       "ninja":("ninja","--version",(1,10)),
       "pkg-config":("pkgconf","--version",(1,8)),
       "nasm":("nasm","-v",(2,15)),
       "meson":(None,"--version",(1,12,1))}


def require(condition, message):
    if not condition: raise RuntimeError(message)


def inspect_tool(name: str, path: Path) -> dict | None:
    if not path.is_file() or not os.access(path,os.X_OK): return None
    result=subprocess.run([str(path),TOOLS[name][1]],text=True,capture_output=True,timeout=20)
    output=result.stdout.strip()
    match=re.search(r"\b([0-9]+(?:\.[0-9]+)+)\b",output.splitlines()[0] if output else "")
    if result.returncode or not match: return None
    version=tuple(map(int,match[1].split(".")))
    minimum=TOOLS[name][2]
    if (version+(0,)*4)[:4] < (minimum+(0,)*4)[:4]: return None
    return {"path":str(path.absolute()),"real_path":str(path.resolve()),"version":match[1],
            "version_output":output,"sha256":hashlib.sha256(path.read_bytes()).hexdigest()}


def find_tool(name: str) -> dict | None:
    candidates=[]
    if path:=shutil.which(name): candidates.append((Path(path),"runner-preinstalled"))
    # The native macOS runner also ships CMake/Ninja in its Android SDK. Those
    # are host executables, not Android-target binaries or cross compilers.
    if name in ("cmake","ninja") and (sdk:=os.environ.get("ANDROID_HOME")):
        candidates.extend((p,"runner-android-sdk") for p in sorted((Path(sdk)/"cmake").glob("*/bin/"+name),reverse=True))
    formula=TOOLS[name][0]
    if formula:
        prefix=subprocess.run(["brew","--prefix",formula],text=True,capture_output=True,timeout=20)
        if prefix.returncode==0:
            candidates.append((Path(prefix.stdout.strip())/"bin"/name,"homebrew-installed"))
    for path,provider in candidates:
        if record:=inspect_tool(name,path): return {**record,"provider":provider,"formula":formula}
    return None


def verify_receipt(path: Path) -> dict:
    require(path.is_file() and path.stat().st_size<=256*1024,"Missing or oversized macOS tool receipt")
    receipt=json.loads(path.read_text())
    require(receipt.get("schema")==1 and receipt.get("platform")=="Darwin" and
            set(receipt.get("tools",{}))==set(TOOLS),"Invalid macOS tool receipt")
    for name,record in receipt["tools"].items():
        active=shutil.which(name)
        current=inspect_tool(name,Path(active)) if active else None
        require(current is not None and all(current[k]==record[k] for k in
                ("real_path","version","version_output","sha256")),f"Executed {name} differs from its prepared tool receipt")
    meson=receipt["tools"]["meson"]
    require(meson.get("provider")=="official-pypi-wheel" and meson["version"]==MESON_VERSION and
            meson.get("wheel_url")==MESON_URL and meson.get("wheel_sha256")==MESON_SHA256,
            "Meson differs from the fixed official wheel")
    return receipt


def prepare(work: Path) -> Path:
    require(platform.system()=="Darwin" and sys.version_info>=(3,11),"Native macOS and existing Python 3.11+ are required")
    root=Path(os.environ["RUNNER_TEMP"]).resolve()
    work=work.resolve()
    require(work!=root and work.is_relative_to(root) and not work.exists(),"Tool work directory must be new and inside RUNNER_TEMP")
    require(not any(c in str(work) for c in "\r\n"),"Invalid tool directory")
    work.mkdir(parents=True)
    selected={name:find_tool(name) for name in TOOLS if name!="meson"}
    missing=[name for name,record in selected.items() if record is None]
    if missing:
        for name in missing:
            installed=subprocess.run(["brew","list","--formula","--versions",TOOLS[name][0]],capture_output=True,text=True,timeout=20)
            require(not installed.stdout.strip(),f"Installed {name} is below the required version; select a qualified runner tool explicitly")
        environment=dict(os.environ,HOMEBREW_NO_AUTO_UPDATE="1",HOMEBREW_NO_INSTALL_UPGRADE="1",HOMEBREW_NO_INSTALL_CLEANUP="1")
        subprocess.run(["brew","install","--formula",*(TOOLS[name][0] for name in missing)],env=environment,check=True,timeout=1800)
        for name in missing:
            selected[name]=find_tool(name)
            require(selected[name] is not None,f"Installed {name} does not meet the native build requirements")
    wheel=work/MESON_WHEEL
    with urllib.request.urlopen(MESON_URL,timeout=45) as response:
        require(response.geturl().startswith("https://files.pythonhosted.org/"),"Meson download left its official package host")
        data=response.read(4*1024*1024+1)
    require(len(data)<=4*1024*1024 and hashlib.sha256(data).hexdigest()==MESON_SHA256,"Official Meson wheel checksum mismatch")
    wheel.write_bytes(data)
    venv=work/"venv"
    subprocess.run([sys.executable,"-m","venv",str(venv)],check=True,timeout=60)
    subprocess.run([str(venv/"bin/python"),"-m","pip","install","--no-index","--no-deps","--disable-pip-version-check","--no-compile",str(wheel)],check=True,timeout=60)
    meson=inspect_tool("meson",venv/"bin/meson")
    require(meson is not None and meson["version"]==MESON_VERSION,"Installed Meson version differs from the official wheel")
    selected["meson"]={**meson,"provider":"official-pypi-wheel","wheel_url":MESON_URL,"wheel_sha256":MESON_SHA256}
    binaries=work/"bin";binaries.mkdir()
    for name,record in selected.items(): (binaries/name).symlink_to(record["path"])
    receipt=work/"tools.json"
    receipt.write_text(json.dumps({"schema":1,"platform":"Darwin","python":{"path":sys.executable,"version":platform.python_version()},
                                   "runner_image":os.environ.get("ImageVersion"),
                                   "runner_source":"https://github.com/actions/runner-images",
                                   "homebrew_source":"https://github.com/Homebrew/homebrew-core","tools":selected},indent=2)+"\n")
    os.environ["PATH"]=str(binaries)+os.pathsep+os.environ["PATH"]
    verify_receipt(receipt)
    return receipt


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work",type=Path,required=True)
    args=parser.parse_args()
    receipt=prepare(args.work)
    with open(os.environ["GITHUB_PATH"],"a",encoding="utf-8") as output: output.write(str(receipt.parent/"bin")+"\n")
    with open(os.environ["GITHUB_ENV"],"a",encoding="utf-8") as output: output.write("BILIBILIVE_MACOS_TOOLS_RECEIPT="+str(receipt)+"\n")
    print(json.dumps({"status":"prepared-and-verified","receipt":str(receipt)}))


if __name__=="__main__": main()
