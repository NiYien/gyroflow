# SPDX-License-Identifier: GPL-3.0-or-later

"""Exercise the production Pascal downloader in a registry-free Inno harness."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import unittest
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


ROOT = Path(__file__).resolve().parents[2]
SETUP_DIR = ROOT / "_deployment" / "windows"
PACKAGE_NAME = "gyroflow-niyien-windows64.zip"


def zip_bytes(name, data):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(name, data)
    return stream.getvalue()


def find_compiler():
    configured = os.environ.get("NIYIEN_ISCC")
    if configured:
        return Path(configured)
    for root in ("LOCALAPPDATA", "ProgramFiles(x86)", "ProgramFiles"):
        candidate = Path(os.environ.get(root, "."))
        if root == "LOCALAPPDATA":
            candidate /= "Programs"
        candidate /= "Inno Setup 6/ISCC.exe"
        if candidate.is_file():
            return candidate
    return None


@unittest.skipUnless(sys.platform == "win32", "Requires Windows PowerShell")
class WindowsSetupBuildConfigurationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.output = ROOT / "target" / "windows-setup-routing-tests" / f"build-{time.time_ns()}"
        cls.output.mkdir(parents=True)
        source = (ROOT / "_scripts" / "windows.just").read_text(encoding="utf-8")
        cls.define_function = source.split("    function New-IsppStringDefine {", 1)[1].split("    function Get-Sha256Hex", 1)[0]
        cls.define_function = "function New-IsppStringDefine {" + cls.define_function
        cls.url_block = source.split("        $PackageTag = $Env:NIYIEN_SETUP_PACKAGE_TAG", 1)[1].split("        $Iscc = Get-InnoSetupCompiler", 1)[0]
        cls.url_block = "$PackageTag = $Env:NIYIEN_SETUP_PACKAGE_TAG" + cls.url_block
        cls.url_block = cls.url_block.replace("{{ArtifactPrefix}}", "gyroflow-niyien")

    def build_config(self, **env_updates):
        result_path = self.output / (self._testMethodName + ".json")
        script_path = result_path.with_suffix(".ps1")
        script = "$ErrorActionPreference = 'Stop'\n" + self.define_function
        script += f"\n$ProjectDir = '{ROOT}'\n$PackageName = '{PACKAGE_NAME}'\n$CanonicalVersion = '1.6.3-ni.42'\n"
        script += self.url_block
        script += "\n$result = @{ manifest = $SetupManifestUrl; cn = $PackageCnUrl; global = $PackageGlobalUrl; override = $DefaultPackageUrl; empty_define = (New-IsppStringDefine 'PackageUrl' '') }\n"
        script += f"$result | ConvertTo-Json | Set-Content -LiteralPath '{result_path}' -Encoding UTF8\n"
        script_path.write_text(script, encoding="utf-8")
        environment = os.environ.copy()
        for key in ("NIYIEN_SETUP_PACKAGE_TAG", "NIYIEN_SETUP_PACKAGE_URL", "GITHUB_REF_TYPE", "GITHUB_REF_NAME", "GITHUB_RUN_ID"):
            environment.pop(key, None)
        environment.update(env_updates)
        environment["PATH"] = str(Path(sys.executable).parent) + os.pathsep + environment.get("PATH", "")
        powershell = Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
        result = subprocess.run([str(powershell), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(script_path)],
                                env=environment, capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        configuration = json.loads(result_path.read_text(encoding="utf-8-sig"))
        self.assertEqual(configuration["empty_define"], '/DPackageUrl=""')
        self.assertIn("app_version=1.6.3-ni.42", configuration["manifest"])
        return configuration

    def test_actions_build_pins_both_sources_to_its_run(self):
        configuration = self.build_config(GITHUB_RUN_ID="42", GITHUB_REF_TYPE="branch")
        self.assertEqual(configuration["global"], "https://nightly.link/NiYien/gyroflow/actions/runs/42/gyroflow-niyien-windows.zip")
        self.assertIn(f"/run-42/{PACKAGE_NAME}", configuration["cn"])
        self.assertEqual(configuration["override"], "")

    def test_release_build_pins_both_sources_to_its_tag(self):
        configuration = self.build_config(GITHUB_RUN_ID="999", GITHUB_REF_TYPE="tag", GITHUB_REF_NAME="v1.6.3-ni.42")
        self.assertEqual(configuration["global"], f"https://github.com/NiYien/gyroflow/releases/download/v1.6.3-ni.42/{PACKAGE_NAME}")
        self.assertIn(f"/v1.6.3-ni.42/{PACKAGE_NAME}", configuration["cn"])

    def test_local_build_uses_canonical_version(self):
        configuration = self.build_config()
        self.assertIn(f"/v1.6.3-ni.42/{PACKAGE_NAME}", configuration["cn"])
        self.assertIn(f"/v1.6.3-ni.42/{PACKAGE_NAME}", configuration["global"])

    def test_explicit_build_tag_and_url_are_preserved(self):
        configuration = self.build_config(NIYIEN_SETUP_PACKAGE_TAG="run-56", GITHUB_RUN_ID="999",
                                          NIYIEN_SETUP_PACKAGE_URL="https://example.test/pinned.zip")
        self.assertIn("/actions/runs/56/", configuration["global"])
        self.assertIn(f"/run-56/{PACKAGE_NAME}", configuration["cn"])
        self.assertEqual(configuration["override"], "https://example.test/pinned.zip")


@unittest.skipUnless(sys.platform == "win32", "Requires Windows and Inno Setup")
class WindowsSetupRoutingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compiler = find_compiler()
        if cls.compiler is None:
            raise unittest.SkipTest("Inno Setup compiler was not found")
        cls.output = ROOT / "target" / "windows-setup-routing-tests" / str(time.time_ns())
        cls.output.mkdir(parents=True)
        cls.package = zip_bytes("Gyroflow.exe", b"pinned-build-42")
        cls.sha256 = hashlib.sha256(cls.package).hexdigest()
        cls.wrapper = zip_bytes(PACKAGE_NAME, cls.package)
        cls.requests = []
        cls.responses = {}

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                cls.requests.append(self.path)
                value = cls.responses.get(self.path)
                if isinstance(value, list):
                    value = value.pop(0) if len(value) > 1 else value[0]
                status, body = value if value else (404, b"missing fixture")
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.server_thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.server_thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_port}"
        cls.cn_path = f"/123/run-42/{PACKAGE_NAME}"
        cls.global_path = "/github/runs/42/gyroflow-niyien-windows.zip"
        cls.release_path = f"/github/releases/v1.6.3-ni.42/{PACKAGE_NAME}"
        source = (SETUP_DIR / "gyroflow-niyien-setup.iss").read_text(encoding="utf-8")
        preamble = source.split("[Setup]", 1)[0]
        messages = source.split("[CustomMessages]", 1)[1].split("[Tasks]", 1)[0]
        code = source.split("[Code]", 1)[1]
        code = code.replace("function InitializeSetup:", "function InitializeProductionSetup:")
        code = code.replace("function NextButtonClick(", "function ProductionNextButtonClick(")
        harness = preamble + r"""
[Setup]
AppName=Gyroflow installer routing regression
AppVersion=1.0
DefaultDirName={tmp}\unused-routing-test
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
ArchiveExtraction=full
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
SetupLogging=yes
[CustomMessages]
""" + messages + "\n[Code]\n" + code + r"""

function InitializeSetup: Boolean;
var
  JsonText: AnsiString;
  Outcome: String;
begin
  Result := InitializeProductionSetup;
  if HasSwitch('JSONTEST') then
  begin
    if not LoadStringFromFile(GetSwitchValue('JSONTEST', ''), JsonText) then
      RaiseException('Failed to read the JSON fixture.');
    try
      Outcome := ReadManifestSource(String(JsonText));
    except
      Outcome := 'error:' + GetExceptionMessage;
    end;
    SaveStringToFile(GetSwitchValue('RESULTFILE', ''), Outcome, False);
    Result := False;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultPath: String;
begin
  ResultPath := GetSwitchValue('RESULTFILE', '');
  Result := '';
  if DownloadAndVerifyPackage then
  begin
    if not FileCopy(ExpandConstant('{tmp}\{#PackageFilename}'), ResultPath + '.zip', False) then
      RaiseException('Failed to retain the staged test package.');
    SaveStringToFile(ResultPath, 'ok', False);
  end
  else
  begin
    SaveStringToFile(ResultPath, 'failed', False);
    Result := 'Package preparation failed.';
  end;
end;
"""
        cls.harness = cls.output / "routing-harness.iss"
        cls.harness.write_text(harness, encoding="utf-8")
        cls.compile_harness("artifact", cls.global_path)
        cls.compile_harness("release", cls.release_path)
        cls.compile_harness("override", cls.global_path, cls.base + cls.cn_path)

    @classmethod
    def compile_harness(cls, name, global_path, explicit_url=""):
        definitions = {
            "PackageTag": "run-42" if name != "release" else "v1.6.3-ni.42",
            "PackageSha256": cls.sha256,
            "ManifestUrl": cls.base + "/manifest",
            "PackageCnUrl": cls.base + cls.cn_path,
            "PackageGlobalUrl": cls.base + global_path,
            "PackageUrl": explicit_url,
        }
        command = [str(cls.compiler), "/Q", f"/O{cls.output}", f"/F{name}", f"/I{SETUP_DIR}"]
        command += [f"/D{key}={value}" for key, value in definitions.items()]
        command.append(str(cls.harness))
        result = subprocess.run(command, capture_output=True, text=True, timeout=60)
        if result.returncode:
            raise RuntimeError(result.stdout + result.stderr)

    @classmethod
    def tearDownClass(cls):
        if hasattr(cls, "server"):
            cls.server.shutdown()
            cls.server.server_close()
            cls.server_thread.join(timeout=5)
        if hasattr(cls, "output"):
            print(f"Installer regression logs and staged packages: {cls.output}")

    def setUp(self):
        self.requests.clear()
        self.responses.clear()
        self.responses.update({
            self.cn_path: (200, self.package),
            self.global_path: (200, self.wrapper),
            self.release_path: (200, self.package),
        })

    def manifest(self, source):
        # The auto-update version deliberately differs from the installer build.
        return json.dumps({
            "selected_source": source,
            "app": {"version": "1.6.3-ni.999", "packages": {"windows": {
                "package_url": self.base + "/latest-wrong-version.zip",
            }}},
        }).encode()

    def run_setup(self, name="artifact", switches=()):
        result_path = self.output / (self._testMethodName + ".result")
        log_path = result_path.with_suffix(".log")
        command = [str(self.output / f"{name}.exe"), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                   f"/RESULTFILE={result_path}", f"/LOG={log_path}", *switches]
        result = subprocess.run(command, capture_output=True, timeout=40)
        if result.returncode in (0xC0000022, -1073741790):
            raise RuntimeError("Windows denied execution of the compiled harness; stop and retry outside the sandbox")
        self.assertTrue(result_path.is_file(), f"No result file, exit={result.returncode}; log={log_path}")
        outcome = result_path.read_text(encoding="utf-8-sig")
        if outcome == "ok":
            self.assertEqual(result_path.with_suffix(".result.zip").read_bytes(), self.package)
        return outcome, log_path.read_text(encoding="utf-8-sig", errors="replace")

    def test_cn_downloads_pinned_package(self):
        self.responses["/manifest"] = (200, self.manifest("cn"))
        outcome, log = self.run_setup()
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, ["/manifest", self.cn_path])

    def test_global_unwraps_pinned_actions_package(self):
        self.responses["/manifest"] = (200, self.manifest("global"))
        outcome, log = self.run_setup()
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, ["/manifest", self.global_path])

    def test_global_release_uses_raw_pinned_zip(self):
        self.responses["/manifest"] = (200, self.manifest("global"))
        outcome, log = self.run_setup("release")
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, ["/manifest", self.release_path])

    def test_existing_updater_local_package_skips_manifest(self):
        local_package = self.output / "updater-local.zip"
        local_package.write_bytes(self.package)
        outcome, log = self.run_setup(switches=("/UPDATE=1", f"/PACKAGEFILE={local_package}",
                                              f"/PACKAGEURL={self.base}/unavailable.zip",
                                              f"/PACKAGESHA256={self.sha256}"))
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, [])

    def test_existing_updater_explicit_url_skips_manifest(self):
        outcome, log = self.run_setup(switches=("/UPDATE=1", f"/PACKAGEURL={self.base}{self.cn_path}",
                                              f"/PACKAGESHA256={self.sha256}"))
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, [self.cn_path])

    def test_existing_updater_explicit_actions_url_is_unwrapped(self):
        outcome, log = self.run_setup(switches=("/UPDATE=1", f"/PACKAGEURL={self.base}{self.global_path}"))
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, [self.global_path])

    def test_build_time_url_override_is_preserved(self):
        outcome, log = self.run_setup("override")
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, [self.cn_path])

    def test_bad_local_package_does_not_fall_back_to_network(self):
        local_package = self.output / "updater-bad-local.zip"
        local_package.write_bytes(b"wrong build")
        outcome, _ = self.run_setup(switches=("/UPDATE=1", f"/PACKAGEFILE={local_package}"))
        self.assertEqual(outcome, "failed")
        self.assertEqual(self.requests, [])

    def test_wrong_actions_inner_package_is_rejected(self):
        self.responses["/manifest"] = (200, self.manifest("global"))
        self.responses[self.global_path] = (200, zip_bytes(PACKAGE_NAME, b"different version"))
        outcome, log = self.run_setup()
        self.assertEqual(outcome, "failed", log)
        self.assertEqual(self.requests.count(self.global_path), 3)
        self.assertIn("SHA256 mismatch", log)

    def test_actions_wrapper_must_contain_expected_filename(self):
        self.responses["/manifest"] = (200, self.manifest("global"))
        self.responses[self.global_path] = (200, zip_bytes("other.zip", self.package))
        outcome, log = self.run_setup()
        self.assertEqual(outcome, "failed", log)
        self.assertIn("does not contain", log)

    def test_manifest_failure_retries_before_download(self):
        self.responses["/manifest"] = [(503, b"temporary failure"), (200, self.manifest("cn"))]
        outcome, log = self.run_setup()
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, ["/manifest", "/manifest", self.cn_path])

    def test_unknown_manifest_source_fails_without_downloading(self):
        self.responses["/manifest"] = (200, self.manifest("unknown"))
        outcome, log = self.run_setup()
        self.assertEqual(outcome, "failed", log)
        self.assertEqual(self.requests, ["/manifest"] * 3)

    def test_manifest_parser_ignores_nested_keys_and_escaped_strings(self):
        self.responses["/manifest"] = (200, b'{"note":"selected_source", "app":{"selected_source":"cn","notes":"brace } and \\\"quote\\\""}, "selected_source" : "global"}')
        outcome, log = self.run_setup()
        self.assertEqual(outcome, "ok", log)
        self.assertEqual(self.requests, ["/manifest", self.global_path])

    def test_manifest_parser_rejects_missing_duplicate_and_truncated_sources(self):
        fixtures = [b'{}', b'{"app":{"selected_source":"cn"}}',
                    b'{"selected_source":"cn","selected_source":"global"}',
                    b'{"selected_source":"cn"', b'{"selected_source":null}',
                    b'{"selected_source":"cn"} trailing']
        for index, fixture in enumerate(fixtures):
            with self.subTest(fixture=fixture):
                json_path = self.output / f"invalid-{index}.json"
                json_path.write_bytes(fixture)
                outcome, _ = self.run_setup(switches=(f"/JSONTEST={json_path}",))
                self.assertTrue(outcome.startswith("error:"), outcome)

    def test_manifest_parser_accepts_unicode_escaped_source_key(self):
        json_path = self.output / "escaped-key.json"
        json_path.write_bytes(b'{"\\u0073elected_source":"global"}')
        outcome, _ = self.run_setup(switches=(f"/JSONTEST={json_path}",))
        self.assertEqual(outcome, "global")


if __name__ == "__main__":
    unittest.main(verbosity=2)
