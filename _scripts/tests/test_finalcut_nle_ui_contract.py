import unittest
import xml.etree.ElementTree as ET
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
QML = ROOT / "src" / "ui" / "menu" / "NlePlugins.qml"
CONTROLLER = ROOT / "src" / "controller.rs"
BACKEND = ROOT / "src" / "nle_plugins.rs"
TRANSLATIONS = ROOT / "resources" / "translations"

FINALCUT_SOURCES = (
    "Installed",
    "Update available",
    "Template missing",
    "Broken or untrusted",
    "Not installed",
    "Repair",
    (
        "Unable to replace the Final Cut integration while related apps may be using it.\n"
        "Close Final Cut Pro, Motion, and Gyroflow NiYien Final Cut, then click Repair or Install again."
    ),
    (
        "The Final Cut App was installed, but its Motion template could not be verified.\n"
        "Close Final Cut Pro and Motion, then click Repair again."
    ),
    (
        "The downloaded Final Cut integration could not be verified as trusted. No App was installed.\n"
        "Check your network connection and try again later."
    ),
    (
        "Final Cut integration installed.\n"
        "Close and reopen Final Cut Pro before using the effect."
    ),
)


def context_messages(catalog: Path) -> dict[str, ET.Element]:
    root = ET.parse(catalog).getroot()
    context = next(
        item for item in root.findall("context") if item.findtext("name") == "NlePlugins"
    )
    return {item.findtext("source", default=""): item for item in context.findall("message")}


class FinalCutNleUiContractTests(unittest.TestCase):
    def test_finalcut_row_is_macos_only_and_exposes_all_five_states(self):
        qml = QML.read_text(encoding="utf-8")

        self.assertIn('readonly property bool finalcutSupported: Qt.platform.os === "osx";', qml)
        self.assertIn("visible: root.finalcutSupported;", qml)
        self.assertIn('controller.nle_plugins("status", "finalcut")', qml)
        self.assertIn('controller.nle_plugins("install", "finalcut")', qml)
        for state in (
            "installed",
            "update_available",
            "app_installed_template_missing",
            "broken_or_untrusted",
        ):
            self.assertIn(f'case "{state}"', qml)
        self.assertIn('return qsTr("Not installed")', qml)

    def test_finalcut_actions_and_feedback_are_textual_and_request_scoped(self):
        qml = QML.read_text(encoding="utf-8")

        self.assertIn('return qsTr("Install")', qml)
        self.assertIn('return qsTr("Update")', qml)
        self.assertIn('return qsTr("Repair")', qml)
        self.assertIn('root.initiatedInstallType = "finalcut"', qml)
        self.assertIn('root.loader && root.initiatedInstallType === "finalcut"', qml)
        self.assertEqual(qml.count('qsTr("Final Cut integration installed.'), 1)

    def test_known_finalcut_failures_have_recovery_guidance(self):
        qml = QML.read_text(encoding="utf-8")

        for code in (
            "FINALCUT_APP_INSTALL_BLOCKED:",
            "FINALCUT_TEMPLATE_INSTALL_FAILED:",
            "FINALCUT_INSTALL_VERIFICATION_FAILED:",
        ):
            self.assertIn(code, qml)
        for product in ("Final Cut Pro", "Motion", "Gyroflow NiYien Final Cut"):
            self.assertIn(product, qml)
        self.assertIn("Check your network connection and try again later.", qml)

    def test_host_detection_uses_launch_services_then_validated_fallbacks(self):
        controller = CONTROLLER.read_text(encoding="utf-8")
        backend = BACKEND.read_text(encoding="utf-8")

        self.assertIn("is_final_cut_host_installed()", controller)
        self.assertIn("NSWorkspace", backend)
        self.assertIn("URLForApplicationWithBundleIdentifier", backend)
        self.assertIn('"com.apple.FinalCutApp"', backend)
        self.assertIn('"com.apple.FinalCut"', backend)
        self.assertIn('"/Applications/Final Cut Pro Creator Studio.app"', backend)
        self.assertIn('"/Applications/Final Cut Pro.app"', backend)
        self.assertIn("fn final_cut_host_bundle_identifier", backend)
        self.assertIn('&path.join("Contents").join("Info.plist"),', backend)
        self.assertIn('"CFBundleIdentifier"', backend)
        self.assertIn("valid_final_cut_host_bundle", backend)

    def test_finalcut_strings_are_finished_in_all_language_catalogs(self):
        catalogs = sorted(TRANSLATIONS.glob("*.ts"))
        self.assertEqual(len(catalogs), 23)

        for catalog in catalogs:
            with self.subTest(catalog=catalog.name):
                messages = context_messages(catalog)
                for source in FINALCUT_SOURCES:
                    self.assertIn(source, messages)
                    translation = messages[source].find("translation")
                    self.assertIsNotNone(translation)
                    if catalog.name == "gyroflow.ts":
                        self.assertEqual(translation.get("type"), "unfinished")
                        continue
                    self.assertNotEqual(translation.get("type"), "unfinished")
                    translated = "".join(translation.itertext()).strip()
                    self.assertTrue(translated)
                    for product in ("Final Cut Pro", "Motion", "Gyroflow NiYien Final Cut"):
                        if product in source:
                            self.assertIn(product, translated)
                    if ".gyroflow" in source:
                        self.assertIn(".gyroflow", translated)

    def test_every_catalog_has_a_nonempty_runtime_qm(self):
        for catalog in sorted(TRANSLATIONS.glob("*.ts")):
            with self.subTest(catalog=catalog.name):
                runtime = catalog.with_suffix(".qm")
                self.assertTrue(runtime.is_file())
                self.assertGreater(runtime.stat().st_size, 0)


if __name__ == "__main__":
    unittest.main()
