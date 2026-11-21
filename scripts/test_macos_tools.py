#!/usr/bin/env python3
"""Offline fixtures for reusing qualified tools without Homebrew upgrades."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location("macos_tools",Path(__file__).with_name("prepare-macos-tools.py"))
mac=importlib.util.module_from_spec(spec);spec.loader.exec_module(mac)


class MacToolsTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name).resolve()
    def tearDown(self): self.temp.cleanup()

    def executable(self,path):
        path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(b"fixture executable");path.chmod(0o755)
        return path

    def test_existing_tool_must_meet_native_build_version(self):
        path=self.executable(self.root/"cmake")
        for output,qualified in (("cmake version 3.27.9\n",False),("cmake version 3.28.0\nCMake suite",True),("unknown",False)):
            with self.subTest(output=output),patch.object(mac.subprocess,"run",return_value=subprocess.CompletedProcess([],0,output,"")):
                record=mac.inspect_tool("cmake",path)
                self.assertEqual(record is not None,qualified)
                if record:self.assertEqual(record["sha256"],hashlib.sha256(path.read_bytes()).hexdigest())
        with patch.object(mac.subprocess,"run",return_value=subprocess.CompletedProcess([],1,"cmake version 4.1.2","failure")):
            self.assertIsNone(mac.inspect_tool("cmake",path))

    def test_android_sdk_host_cmake_is_reused_when_path_is_too_old(self):
        old=self.executable(self.root/"old/cmake")
        sdk=self.executable(self.root/"sdk/cmake/3.31.5/bin/cmake")
        def run(args,**kwargs):
            output="cmake version 3.27.9" if args[0]==str(old) else "cmake version 3.31.5"
            return subprocess.CompletedProcess(args,1 if args[0]=="brew" else 0,output,"")
        with patch.dict(os.environ,{"ANDROID_HOME":str(self.root/"sdk")}),patch.object(mac.shutil,"which",return_value=str(old)),patch.object(mac.subprocess,"run",side_effect=run):
            record=mac.find_tool("cmake")
        self.assertEqual(record["real_path"],str(sdk.resolve()))
        self.assertEqual(record["provider"],"runner-android-sdk")

    def selection_until_download(self,find,run):
        # Stop before the network boundary: these fixtures exercise selection
        # and Homebrew policy without claiming any native compilation passed.
        with patch.dict(os.environ,{"RUNNER_TEMP":str(self.root)}),patch.object(mac.platform,"system",return_value="Darwin"),patch.object(mac,"find_tool",side_effect=find),patch.object(mac.subprocess,"run",side_effect=run),patch.object(mac.urllib.request,"urlopen",side_effect=LookupError("download boundary")):
            with self.assertRaisesRegex(LookupError,"download boundary"):
                mac.prepare(self.root/"tools")

    def test_qualified_preinstalled_tools_never_trigger_brew_install(self):
        calls=[]
        def run(args,**kwargs): calls.append(args);raise AssertionError("No installer should run")
        self.selection_until_download(lambda name:{"path":name},run)
        self.assertEqual(calls,[])

    def test_only_missing_tool_is_installed_with_upgrades_disabled(self):
        calls=[];seen={}
        def find(name):
            seen[name]=seen.get(name,0)+1
            return None if name=="nasm" and seen[name]==1 else {"path":name}
        def run(args,**kwargs):
            calls.append((args,kwargs));return subprocess.CompletedProcess(args,0,"","")
        self.selection_until_download(find,run)
        self.assertEqual([args for args,kwargs in calls],[['brew','list','--formula','--versions','nasm'],['brew','install','--formula','nasm']])
        environment=calls[-1][1]["env"]
        for name in ("HOMEBREW_NO_AUTO_UPDATE","HOMEBREW_NO_INSTALL_UPGRADE","HOMEBREW_NO_INSTALL_CLEANUP"):
            self.assertEqual(environment[name],"1")

    def test_old_installed_formula_is_not_silently_upgraded(self):
        with patch.dict(os.environ,{"RUNNER_TEMP":str(self.root)}),patch.object(mac.platform,"system",return_value="Darwin"),patch.object(mac,"find_tool",return_value=None),patch.object(mac.subprocess,"run",return_value=subprocess.CompletedProcess([],0,"cmake 3.27.9","")) as run,patch.object(mac.urllib.request,"urlopen") as download:
            with self.assertRaisesRegex(RuntimeError,"below the required version"):
                mac.prepare(self.root/"tools")
        self.assertEqual(run.call_count,1);download.assert_not_called()

    def test_receipt_rejects_changed_executable_and_changed_meson_pin(self):
        paths={name:self.executable(self.root/name) for name in mac.TOOLS}
        versions={"cmake":"3.31.5","ninja":"1.13.2","pkg-config":"3.0.5","nasm":"2.16.03","meson":mac.MESON_VERSION}
        def run(args,**kwargs):return subprocess.CompletedProcess(args,0,versions[Path(args[0]).name],"")
        with patch.object(mac.subprocess,"run",side_effect=run),patch.object(mac.shutil,"which",side_effect=lambda name:str(paths[name])):
            records={name:mac.inspect_tool(name,path) for name,path in paths.items()}
            records["meson"].update(provider="official-pypi-wheel",wheel_url=mac.MESON_URL,wheel_sha256=mac.MESON_SHA256)
            receipt=self.root/"tools.json"
            receipt.write_text(json.dumps({"schema":1,"platform":"Darwin","tools":records}))
            mac.verify_receipt(receipt)
            paths["ninja"].write_bytes(b"changed executable")
            with self.assertRaisesRegex(RuntimeError,"ninja differs"):
                mac.verify_receipt(receipt)
            paths["ninja"].write_bytes(b"fixture executable")
            records["meson"]["wheel_sha256"]="0"*64
            receipt.write_text(json.dumps({"schema":1,"platform":"Darwin","tools":records}))
            with self.assertRaisesRegex(RuntimeError,"fixed official wheel"):
                mac.verify_receipt(receipt)


if __name__=="__main__":unittest.main()
