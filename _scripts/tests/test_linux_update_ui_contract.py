import unittest
import xml.etree.ElementTree as ET
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
APP_QML = ROOT / "src" / "ui" / "App.qml"
TRANSLATIONS = ROOT / "resources" / "translations"

LINUX_READY_TEXT = (
    "The AppImage is ready. Open its folder, then launch it when you are ready. "
    "Gyroflow will stay open."
)
LINUX_OPEN_LABEL = "Open containing folder"


class LinuxUpdateUiContractTests(unittest.TestCase):
    def test_linux_ready_action_opens_folder_without_quit_method(self):
        qml = APP_QML.read_text(encoding="utf-8")

        self.assertIn('const isLinux = platform === "linux";', qml)
        self.assertIn(f'qsTr("{LINUX_READY_TEXT}")', qml)
        self.assertIn(f'qsTr("{LINUX_OPEN_LABEL}")', qml)
        self.assertIn("controller.open_downloaded_update()", qml)
        self.assertNotIn("controller.open_downloaded_update_and_quit()", qml)

    def test_linux_update_strings_exist_in_every_translation_catalog(self):
        catalogs = sorted(TRANSLATIONS.glob("*.ts"))
        self.assertGreater(len(catalogs), 1)

        for catalog in catalogs:
            with self.subTest(catalog=catalog.name):
                root = ET.parse(catalog).getroot()
                app_context = next(
                    context
                    for context in root.findall("context")
                    if context.findtext("name") == "App"
                )
                messages = {
                    message.findtext("source"): message
                    for message in app_context.findall("message")
                }
                self.assertIn(LINUX_READY_TEXT, messages)
                self.assertIn(LINUX_OPEN_LABEL, messages)
                if catalog.name != "gyroflow.ts":
                    for source in (LINUX_READY_TEXT, LINUX_OPEN_LABEL):
                        translation = messages[source].find("translation")
                        self.assertIsNotNone(translation)
                        self.assertNotEqual(translation.get("type"), "unfinished")
                        self.assertTrue((translation.text or "").strip())


if __name__ == "__main__":
    unittest.main()
