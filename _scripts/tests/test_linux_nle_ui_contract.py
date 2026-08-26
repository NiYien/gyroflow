import unittest
import xml.etree.ElementTree as ET
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
APP_QML = ROOT / "src" / "ui" / "App.qml"
NLE_QML = ROOT / "src" / "ui" / "menu" / "NlePlugins.qml"
TRANSLATIONS = ROOT / "resources" / "translations"

MANUAL_INSTALL_TEXT = (
    "Automatic installation could not be completed. Open Terminal and run the "
    "following command:"
)


class LinuxNleUiContractTests(unittest.TestCase):
    def test_linux_enables_nle_card_but_hides_adobe(self):
        app_qml = APP_QML.read_text(encoding="utf-8")
        nle_qml = NLE_QML.read_text(encoding="utf-8")

        self.assertIn('Qt.platform.os === "linux"', app_qml)
        self.assertIn(
            'readonly property bool adobeSupported: Qt.platform.os !== "linux";',
            nle_qml,
        )
        self.assertIn("if (root.adobeSupported)", nle_qml)
        self.assertIn("visible: root.adobeSupported;", nle_qml)

    def test_linux_manual_fallback_uses_fixed_destination_without_shell_execution(self):
        qml = NLE_QML.read_text(encoding="utf-8")

        self.assertIn("PLUGIN_MANUAL_INSTALL_REQUIRED:", qml)
        self.assertIn(f'qsTr("{MANUAL_INSTALL_TEXT}")', qml)
        self.assertIn("sudo cp -a --", qml)
        self.assertIn("/usr/OFX/Plugins/", qml)
        self.assertNotIn("sh -c", qml)
        self.assertNotIn("bash -c", qml)

    def test_manual_fallback_text_is_finished_in_every_translation_catalog(self):
        for catalog in sorted(TRANSLATIONS.glob("*.ts")):
            with self.subTest(catalog=catalog.name):
                root = ET.parse(catalog).getroot()
                context = next(
                    item
                    for item in root.findall("context")
                    if item.findtext("name") == "NlePlugins"
                )
                message = next(
                    (
                        item
                        for item in context.findall("message")
                        if item.findtext("source") == MANUAL_INSTALL_TEXT
                    ),
                    None,
                )
                self.assertIsNotNone(message)
                if catalog.name != "gyroflow.ts":
                    translation = message.find("translation")
                    self.assertIsNotNone(translation)
                    self.assertNotEqual(translation.get("type"), "unfinished")
                    self.assertTrue((translation.text or "").strip())


if __name__ == "__main__":
    unittest.main()
