import hashlib
import importlib.util
import stat
import tarfile
import tempfile
import unittest
from pathlib import Path

from _scripts import publish_pan123_release as publish


ROOT = Path(__file__).resolve().parents[2]
VERIFY_SCRIPT = ROOT / "_scripts" / "verify_linux_app_packages.py"
APPIMAGE_NAME = "gyroflow-niyien-linux64.AppImage"
TAR_NAME = "gyroflow-niyien-linux64.tar.gz"


def load_verifier():
    spec = importlib.util.spec_from_file_location("verify_linux_app_packages", VERIFY_SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load {VERIFY_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class LinuxJustContractTests(unittest.TestCase):
    def test_linux_rejects_non_x86_64_before_installing_or_building(self):
        script = (ROOT / "_scripts" / "linux.just").read_text(encoding="utf-8")

        guard = script.index("Unsupported Linux architecture")
        self.assertLess(guard, script.index("sudo apt-get install"))
        self.assertLess(guard, script.index("cargo build"))
        self.assertNotIn("rustup target add aarch64-unknown-linux-gnu", script)
        self.assertNotIn("AppImageBuilder-arm64.yml", script)

    def test_linux_implements_the_shared_desktop_recipe_surface(self):
        script = (ROOT / "_scripts" / "linux.just").read_text(encoding="utf-8")

        for recipe in (
            "run",
            "test",
            "test-core",
            "check",
            "build",
            "build-debug",
            "debug",
            "profile",
            "clippy",
            "install-deps",
            "deploy",
            "bundle",
        ):
            self.assertRegex(script, rf"(?m)^{recipe}(?: \*param)?:")

    def test_linux_justfile_parsing_uses_python3_before_dependency_install(self):
        common = (ROOT / "_scripts" / "common.just").read_text(encoding="utf-8")

        self.assertIn("cd .. && python3 _scripts/niyien_version.py", common)
        self.assertIn("`python3 read_distribution_config.py brand.artifact_prefix`", common)
        self.assertIn("`python3 read_distribution_config.py brand.display_name`", common)

    def test_linux_installs_bootstrap_tools_before_probing_consumers(self):
        script = (ROOT / "_scripts" / "linux.just").read_text(encoding="utf-8")

        self.assertIn("for tool in curl git tar unzip zip; do", script)
        apt_install = script.index("sudo apt-get install -y p7zip-full")
        consumer_probe = script.index("for tool in curl git tar unzip zip; do")
        self.assertLess(apt_install, consumer_probe)
        self.assertIn("for tool in bash cargo python3; do", script)
        self.assertIn(
            "apt install -y sudo dialog apt-utils curl clang python3 git tar unzip zip",
            script,
        )

    def test_linux_discovers_the_distribution_libclang_instead_of_pin_to_13(self):
        script = (ROOT / "_scripts" / "linux.just").read_text(encoding="utf-8")
        common = (ROOT / "_scripts" / "common.just").read_text(encoding="utf-8")

        self.assertNotIn("libclang-13-dev", script)
        self.assertNotIn('/usr/lib/llvm-13/lib/', common)
        self.assertIn("find /usr/lib/llvm-*/lib", script)
        self.assertIn("find /usr/lib/llvm-*/lib", common)

    def test_linux_python_bootstrap_uses_the_current_readline_package(self):
        script = (ROOT / "_scripts" / "linux.just").read_text(encoding="utf-8")

        self.assertNotIn("libreadline-gplv2-dev", script)
        self.assertIn("build-essential libreadline-dev", script)

    def test_linux_installs_node_for_the_release_automation_test_suite(self):
        script = (ROOT / "_scripts" / "linux.just").read_text(encoding="utf-8")

        dependency_line = next(
            line for line in script.splitlines() if "python3-pip" in line
        )
        self.assertIn("nodejs", dependency_line)
        self.assertIn("python3-requests", dependency_line)

    def test_linux_deploy_uses_the_branded_builder_output_and_explicit_final_name(self):
        script = (ROOT / "_scripts" / "linux.just").read_text(encoding="utf-8")

        self.assertIn(
            'BUILDER_APPIMAGE="$TARGET/../Gyroflow(NiYien)-${APP_VERSION}-{{ArchName}}.AppImage"',
            script,
        )
        self.assertIn(
            'ARCH={{Arch}} appimagetool ./Gyroflow.AppDir "{{ArtifactPrefix}}-{{PackageSuffix}}.AppImage"',
            script,
        )
        self.assertNotIn("mv Gyroflow-{{ArchName}}.AppImage", script)


class LinuxWorkflowContractTests(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")

    def test_workflow_builds_one_linux_x86_64_target(self):
        self.assertEqual(self.workflow.count("type: linux"), 1)
        self.assertIn("{ os: ubuntu-22.04,   type: linux }", self.workflow)
        self.assertIn("run: just deploy docker", self.workflow)

    def test_workflow_publishes_both_linux_assets_everywhere(self):
        for filename in (APPIMAGE_NAME, TAR_NAME):
            self.assertIn(f"name: {filename}", self.workflow)
            self.assertIn(f"path: _deployment/_binaries/{filename}", self.workflow)
            self.assertIn(f"./release_artifacts/{filename}", self.workflow)
        self.assertIn("name: gyroflow-niyien-linux-appimage", self.workflow)
        self.assertIn("name: gyroflow-niyien-linux-tar", self.workflow)

    def test_workflow_smoke_checks_linux_packages(self):
        self.assertIn("name: Smoke test Linux packages", self.workflow)
        self.assertIn("verify_linux_app_packages.py", self.workflow)
        self.assertIn("--appimage-extract", self.workflow)
        self.assertIn("ldd", self.workflow)
        self.assertIn("--version", self.workflow)


class LinuxPublisherContractTests(unittest.TestCase):
    def test_active_linux_requires_both_assets(self):
        required = publish.derive_required_app_asset_names(
            workflow_text="targets: [{ os: ubuntu-22.04, type: linux }]"
        )
        self.assertIn(APPIMAGE_NAME, required)
        self.assertIn(TAR_NAME, required)

    def test_paused_linux_requires_neither_asset(self):
        required = publish.derive_required_app_asset_names(
            workflow_text="targets: [{ os: windows-2022, type: windows }]"
        )
        self.assertNotIn(APPIMAGE_NAME, required)
        self.assertNotIn(TAR_NAME, required)

    def test_linux_artifact_names_map_to_independent_raw_files(self):
        self.assertEqual(
            publish.APP_ARTIFACT_NAMES_BY_FILE["gyroflow-niyien-linux-appimage"],
            APPIMAGE_NAME,
        )
        self.assertEqual(
            publish.APP_ARTIFACT_NAMES_BY_FILE["gyroflow-niyien-linux-tar"],
            TAR_NAME,
        )
        urls = publish.build_global_artifact_app_urls("run-42", (APPIMAGE_NAME, TAR_NAME))
        self.assertTrue(urls["linux"]["package_url"].endswith("gyroflow-niyien-linux-appimage.zip"))
        self.assertTrue(urls["linux"]["archive_url"].endswith("gyroflow-niyien-linux-tar.zip"))

    def test_linux_metadata_separates_appimage_and_tar(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            appimage = directory / APPIMAGE_NAME
            archive = directory / TAR_NAME
            appimage.write_bytes(b"appimage")
            archive.write_bytes(b"tar")

            packages = publish.build_app_packages_metadata(
                {APPIMAGE_NAME: appimage, TAR_NAME: archive}
            )

        linux = packages["linux"]
        self.assertEqual(linux["kind"], "appimage")
        self.assertEqual(linux["package_filename"], APPIMAGE_NAME)
        self.assertEqual(linux["package_sha256"], hashlib.sha256(b"appimage").hexdigest())
        self.assertEqual(linux["package_size"], len(b"appimage"))
        self.assertEqual(linux["archive_filename"], TAR_NAME)
        self.assertEqual(linux["archive_sha256"], hashlib.sha256(b"tar").hexdigest())
        self.assertEqual(linux["archive_size"], len(b"tar"))

    def test_plugin_asset_contract_includes_linux_openfx_only(self):
        self.assertIn("GyroflowNiyien-OpenFX-linux.zip", publish.PLUGIN_ASSET_NAMES)
        self.assertNotIn("GyroflowNiyien-Adobe-linux.zip", publish.PLUGIN_ASSET_NAMES)
        self.assertEqual(len(publish.PLUGIN_ASSET_NAMES), 5)


class LinuxPackageProducerTests(unittest.TestCase):
    def setUp(self):
        self.verifier = load_verifier()

    def create_package_pair(self, directory: Path):
        appimage = directory / APPIMAGE_NAME
        appimage.write_bytes(b"\x7fELFappimage")
        appimage.chmod(0o755)

        archive = directory / TAR_NAME
        payload = directory / "gyroflow-niyien"
        payload.write_bytes(b"\x7fELFbinary")
        payload.chmod(0o755)
        with tarfile.open(archive, "w:gz") as tar:
            info = tar.gettarinfo(payload, "Gyroflow/gyroflow-niyien")
            info.mode = 0o755
            with payload.open("rb") as source:
                tar.addfile(info, source)
            for name, contents in (
                ("Gyroflow/lib/libQt6Core.so.6", b"qt"),
                ("Gyroflow/camera_presets/profiles.cbor.gz", b"presets"),
                ("Gyroflow/camera_db/cameras.json", b"{}"),
            ):
                temp = directory / Path(name).name
                temp.write_bytes(contents)
                tar.add(temp, arcname=name)
        return appimage, archive

    def test_complete_package_pair_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            appimage, archive = self.create_package_pair(Path(directory))

            metadata = self.verifier.verify_packages(appimage, archive, require_host_executable=False)

        self.assertGreater(metadata["appimage_size"], 0)
        self.assertIn("Gyroflow/gyroflow-niyien", metadata["tar_members"])

    def test_missing_required_tar_member_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            appimage, archive = self.create_package_pair(Path(directory))
            replacement_dir = Path(directory) / "replacement"
            replacement_dir.mkdir()
            replacement = replacement_dir / TAR_NAME
            with tarfile.open(archive, "r:gz") as source, tarfile.open(replacement, "w:gz") as target:
                for member in source.getmembers():
                    if member.name.startswith("Gyroflow/camera_db/"):
                        continue
                    fileobj = source.extractfile(member) if member.isfile() else None
                    target.addfile(member, fileobj)

            with self.assertRaisesRegex(ValueError, "camera_db"):
                self.verifier.verify_packages(appimage, replacement, require_host_executable=False)


if __name__ == "__main__":
    unittest.main()
