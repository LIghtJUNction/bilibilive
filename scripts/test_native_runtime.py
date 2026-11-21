#!/usr/bin/env python3
"""Offline native-payload preparation fixtures; no compiler/network/account use."""
import hashlib
import importlib.util
import io
import json
import ntpath
from pathlib import Path
import os
import posixpath
import re
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch
import zipfile

sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location("native_runtime",Path(__file__).with_name("prepare-native-runtime.py"))
native=importlib.util.module_from_spec(spec);spec.loader.exec_module(native)


class NativePayloadTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name).resolve()
    def tearDown(self):self.temp.cleanup()

    def test_archive_names_cannot_escape_or_use_windows_ads(self):
        for name in ("../outside","/absolute","dir/../../escape","C:/windows","inside:file","a\\b"):
            with self.subTest(name=name),self.assertRaises(RuntimeError):native.archive_name(name)

    def test_zip_parent_traversal_and_duplicate_names_rejected(self):
        for label,names in (("escape",["../file"]),("case",["DLL.dll","dll.dll"])):
            archive=self.root/(label+".zip")
            with zipfile.ZipFile(archive,"w") as out:
                for name in names:out.writestr(name,b"fixture")
            with self.assertRaises(RuntimeError):native.extract(archive,self.root/label)
        self.assertFalse((self.root.parent/"file").exists())

    def test_tar_rejects_escaping_link_and_accepts_internal_framework_link(self):
        for index,target in enumerate(("../../elsewhere","/absolute","C:/Windows/file","C:relative","library:stream",r"..\outside")):
            with self.subTest(target=target):
                bad=self.root/f"bad-{index}.tar"
                with tarfile.open(bad,"w") as out:
                    member=tarfile.TarInfo("link");member.type=tarfile.SYMTYPE;member.linkname=target;out.addfile(member)
                with self.assertRaises(RuntimeError):native.extract(bad,self.root/f"bad-{index}")
        good=self.root/"good.tar"
        with tarfile.open(good,"w") as out:
            member=tarfile.TarInfo("Versions/A/library");member.size=4;out.addfile(member,io.BytesIO(b"code"))
            member=tarfile.TarInfo("library");member.type=tarfile.SYMTYPE;member.linkname="Versions/A/library";out.addfile(member)
            member=tarfile.TarInfo("hard-library");member.type=tarfile.LNKTYPE;member.linkname="Versions/A/library";out.addfile(member)
        native.extract(good,self.root/"good")
        self.assertEqual((self.root/"good/library").read_bytes(),b"code")
        self.assertEqual((self.root/"good/hard-library").read_bytes(),b"code")
        if (self.root/"good/library").is_symlink():
            self.assertEqual(os.readlink(self.root/"good/library"),
                             "Versions\\A\\library" if os.name=="nt" else "Versions/A/library")

    def test_tar_filter_keeps_safety_and_normalizes_only_windows_symbolic_targets(self):
        member=tarfile.TarInfo("library");member.type=tarfile.SYMTYPE;member.linkname="Versions/A/library"
        destination=str(self.root/"filter")
        windows=native.sdk_tar_filter(member,destination,windows=True)
        self.assertEqual(windows.linkname,"Versions\\A\\library")
        self.assertEqual(member.linkname,"Versions/A/library")
        native_filtered=tarfile.data_filter(member,destination)
        self.assertEqual(native.sdk_tar_filter(member,destination,windows=False).linkname,native_filtered.linkname)
        hard=tarfile.TarInfo("hard-library");hard.type=tarfile.LNKTYPE;hard.linkname="Versions/A/library"
        self.assertEqual(native.sdk_tar_filter(hard,destination,windows=True).linkname,
                         tarfile.data_filter(hard,destination).linkname)
        escaped=tarfile.TarInfo("outside");escaped.type=tarfile.SYMTYPE;escaped.linkname="../../outside"
        with self.assertRaises(tarfile.FilterError):native.sdk_tar_filter(escaped,destination,windows=True)

    def test_tar_internal_file_link_falls_back_without_symlink_permission(self):
        archive=self.root/"fallback.tar"
        with tarfile.open(archive,"w") as out:
            member=tarfile.TarInfo("nested/library");member.size=4;out.addfile(member,io.BytesIO(b"code"))
            member=tarfile.TarInfo("library");member.type=tarfile.SYMTYPE;member.linkname="nested/library";out.addfile(member)
        with patch.object(tarfile.os,"symlink",side_effect=OSError("fixture: no symlink privilege")):
            native.extract(archive,self.root/"fallback")
        self.assertFalse((self.root/"fallback/library").is_symlink())
        self.assertEqual((self.root/"fallback/library").read_bytes(),b"code")

    def test_pe_architecture_and_truncated_header(self):
        data=bytearray(256);data[:2]=b"MZ";struct.pack_into("<I",data,0x3c,128);data[128:132]=b"PE\0\0";struct.pack_into("<H",data,132,0x8664)
        path=self.root/"fixture.dll";path.write_bytes(data)
        self.assertEqual(native.inspect_binary(path,"x86_64-pc-windows-msvc")["format"],"PE")
        with self.assertRaises(RuntimeError):native.inspect_binary(path,"aarch64-apple-darwin")
        path.write_bytes(b"MZ")
        with self.assertRaises(RuntimeError):native.inspect_binary(path,"x86_64-pc-windows-msvc")

    def test_macho_and_elf_architecture_checks(self):
        path=self.root/"fixture"
        data=bytearray(64);data[:4]=b"\xcf\xfa\xed\xfe";struct.pack_into("<I",data,4,0x0100000c);path.write_bytes(data)
        self.assertEqual(native.inspect_binary(path,"aarch64-apple-darwin")["machine"],0x0100000c)
        with self.assertRaises(RuntimeError):native.inspect_binary(path,"x86_64-apple-darwin")
        data=bytearray(64);data[:6]=b"\x7fELF\x02\x01";struct.pack_into("<H",data,18,62);path.write_bytes(data)
        self.assertEqual(native.inspect_binary(path,"x86_64-unknown-linux-gnu")["format"],"ELF64")

    def test_final_inventory_includes_actual_bytes_and_rejects_symlinks(self):
        root=self.root/"payload";root.mkdir();(root/"runtime").mkdir();(root/"runtime/library").write_bytes(b"library")
        record=native.inventory(root)["runtime/library"]
        self.assertEqual(record["size"],7);self.assertEqual(record["sha256"],hashlib.sha256(b"library").hexdigest())
        if hasattr(os,"symlink"):
            try:os.symlink("library",root/"runtime/link")
            except OSError:return
            with self.assertRaises(RuntimeError):native.inventory(root)

    def test_runtime_basename_collision_preserves_first_file(self):
        first=self.root/"one";second=self.root/"two";output=self.root/"result.dll"
        first.write_bytes(b"first");second.write_bytes(b"second")
        native.copy_regular(first,output)
        with self.assertRaises(RuntimeError):native.copy_regular(second,output)
        self.assertEqual(output.read_bytes(),b"first")

    def test_linux_mode_explicitly_uses_system_media_dependencies(self):
        runtime,receipt=native.prepare_runtime({},"x86_64-unknown-linux-gnu",self.root,self.root,2,None)
        self.assertEqual(runtime,{"mode":"system","executables":{},"libraries":{}})
        self.assertIn("distribution-managed",receipt["delivery"])

    def test_official_ffmpeg_source_selects_highest_stable_release(self):
        body=b'<a href="ffmpeg-8.1.2.tar.xz">x</a><a href="ffmpeg-9.1.tar.xz">y</a>'
        with patch.object(native,"request",return_value=(body,"https://ffmpeg.org/download.html")):
            self.assertEqual(native.latest_ffmpeg(),"9.1")

    def test_github_latest_requires_same_official_repository(self):
        repo=native.REPOS["obs"]
        with patch.object(native,"request",return_value=(b"",repo[:-4]+"/releases/tag/33.0.2")):
            self.assertEqual(native.latest_github(repo),"33.0.2")
        with patch.object(native,"request",return_value=(b"","https://example.invalid/releases/tag/99.0.0")):
            with self.assertRaises(RuntimeError):native.latest_github(repo)

    def test_codec_release_selection_excludes_prerelease_and_custom_forks(self):
        refs="\n".join("a"*40+"\trefs/tags/"+tag for tag in ("v4.2.0","v4.3.0-rc","v4.2.0-cqp-extended","v4.3.0","v5.0.0-beta"))
        with patch.object(native,"run",return_value=refs):
            self.assertEqual(native.latest_tag(native.REPOS["svtav1"],r"v([0-9]+\.[0-9]+\.[0-9]+)"),"v4.3.0")

    def test_encoder_description_cannot_fake_missing_software_encoder(self):
        encoders=" V..... libx264 description\n V..... h264_nvenc mentions libsvtav1\n A..... libopus description\n"
        decoders=" V..... h264 description\n V..... libdav1d description\n A..... opus description\n"
        with self.assertRaisesRegex(RuntimeError,"software encoder"):
            native.media_capabilities(encoders,decoders,"x86_64-pc-windows-msvc")
        actual=encoders+" V..... libsvtav1 description\n"
        value=native.media_capabilities(actual,decoders,"x86_64-unknown-linux-gnu")
        self.assertTrue(value["hardware_encoders"]["h264_nvenc"]["compiled"])
        self.assertFalse(value["hardware_encoders"]["h264_nvenc"]["device_execution_verified"])
        self.assertFalse(value["hardware_encoders"]["av1_qsv"]["compiled"])
        with self.assertRaisesRegex(RuntimeError,"compiled hardware encoder"):
            native.media_capabilities(actual,decoders,"x86_64-pc-windows-msvc")

    def test_private_runtime_dll_wins_over_older_toolchain_package(self):
        prefix=self.root/"private";msys=self.root/"msys";out=self.root/"payload"
        for path in (prefix/"bin",msys/"ucrt64/bin",out/"runtime/bin"):path.mkdir(parents=True)
        (msys/"ucrt64/bin/objdump.exe").write_bytes(b"fixture, never executed")
        (prefix/"bin/codec.dll").write_bytes(b"current official source")
        (msys/"ucrt64/bin/codec.dll").write_bytes(b"older package")
        app=out/"runtime/bin/ffmpeg.exe";app.write_bytes(b"fixture")
        def imports(argv,*args,**kwargs):
            return "DLL Name: codec.dll" if Path(argv[-1]).name=="ffmpeg.exe" else "DLL Name: api-ms-win-crt-runtime-l1-1-0.dll"
        with patch.object(native,"run",side_effect=imports):
            native.windows_dependencies([app],out,[prefix],msys)
        self.assertEqual((out/"runtime/lib/codec.dll").read_bytes(),b"current official source")

    def test_openssl_config_relative_path_excludes_commit_and_cache_names(self):
        cache=self.root/"effects-cache"
        source=cache/"sources/openssl-2f4e3a5fec053940"
        source.mkdir(parents=True)
        plan={"sources":{"datachannel":{"commit":"a"*40},"openssl":{"commit":"2f4e3a5fec05394068cfa7379890ab552483a8a6"}}}
        for target in ("x86_64-pc-windows-msvc","aarch64-apple-darwin","x86_64-apple-darwin"):
            with self.subTest(target=target),patch.object(native,"source_checkout",return_value=source) as checkout,\
                    patch.object(native,"run") as run,patch.object(native,"cmake_build"):
                prefix,receipt=native.prepare_datachannel(plan,target,cache,3)
                configure=run.call_args_list[0]
                cwd=configure.kwargs["cwd"]
                self.assertEqual(configure.args[0][1],source/"Configure")
                self.assertEqual(prefix,cache/"build"/target/"datachannel-prefix")
                self.assertEqual(receipt["openssl_build"]["strategy"],"nested-out-of-tree")
                # Mirror upstream's actual unanchored substitution for both
                # path dialects. Only the .ec filename suffix may match.
                for path_module in (posixpath,ntpath):
                    config=path_module.relpath((source/"crypto/err/openssl.ec").as_posix(),cwd.as_posix())
                    state=path_module.relpath((source/"crypto/err/openssl.txt").as_posix(),cwd.as_posix())
                    self.assertEqual(re.sub(r".ec",".txt",config,count=1),state)
                self.assertEqual(checkout.call_args_list[-1].args,("openssl",plan["sources"]["openssl"],cache))
                self.assertEqual(sum(call.args[0]=="openssl" for call in checkout.call_args_list),2)

    def test_visual_cpp_redistributable_is_not_mislabeled_as_windows_os(self):
        msys=self.root/"msys";out=self.root/"payload";system=self.root/"Windows"
        for path in (msys/"ucrt64/bin",out/"runtime/bin",system/"System32"):path.mkdir(parents=True)
        (msys/"ucrt64/bin/objdump.exe").write_bytes(b"fixture")
        (system/"System32/VCRUNTIME140.dll").write_bytes(b"not an OS guarantee")
        app=out/"runtime/bin/ffmpeg.exe";app.write_bytes(b"fixture")
        with patch.dict(os.environ,{"SystemRoot":str(system)}),patch.object(native,"run",return_value="DLL Name: VCRUNTIME140.dll"):
            with self.assertRaisesRegex(RuntimeError,"Unresolved non-system runtime DLL"):
                native.windows_dependencies([app],out,[],msys)

    def test_process_timeout_is_bounded(self):
        begin=time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            native.run([sys.executable,"-c","import time; time.sleep(30)"],timeout=0.05)
        self.assertLess(time.monotonic()-begin,3)

    @unittest.skipIf(os.name=="nt","POSIX process-group fixture")
    def test_timeout_terminates_owned_grandchild(self):
        pidfile=self.root/"grandchild-pid"
        program=("import subprocess,sys,time,pathlib; "
                 "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); "
                 "pathlib.Path(sys.argv[1]).write_text(str(p.pid)); time.sleep(30)")
        with self.assertRaises(subprocess.TimeoutExpired):native.run([sys.executable,"-c",program,pidfile],timeout=0.2)
        pid=int(pidfile.read_text())
        status=Path(f"/proc/{pid}/stat")
        if status.exists():self.assertEqual(status.read_text().split()[2],"Z")

    def test_output_inside_source_scope_is_rejected_before_network(self):
        with patch.object(native,"resolve") as resolve:
            with self.assertRaises(RuntimeError):native.main(["--output",str(native.ROOT/"plugins/unwanted-binary"),"--cache",str(self.root/"cache"),"--target","x86_64-unknown-linux-gnu","--plan"])
            resolve.assert_not_called()


if __name__=="__main__":unittest.main(verbosity=2)
