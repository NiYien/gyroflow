# SPDX-License-Identifier: GPL-3.0-or-later
"""Keep directory imports usable when Android/iOS contain only compiled QML."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]


class QmlResourceRegistration(unittest.TestCase):
    def test_compiled_qml_directory_imports_have_types_and_resources(self):
        resources = (ROOT / "src/resources.rs").read_text(encoding="utf-8")
        fallback = (ROOT / "src/resources_qml.rs").read_text(encoding="utf-8")
        for folder in (ROOT / "src/ui").iterdir():
            if not folder.is_dir() or not list(folder.glob("*.qml")):
                continue
            with self.subTest(folder=folder.name):
                manifest = folder / "qmldir"
                self.assertTrue(manifest.is_file(), f"Missing directory resource: {manifest}")
                self.assertIn('"' + manifest.relative_to(ROOT).as_posix() + '"', resources)
                exports = set(re.findall(r"^(?:singleton\s+)?\w+\s+[0-9.]+\s+(\S+\.qml)\s*$", manifest.read_text(encoding="utf-8"), re.M))
                for component in folder.glob("*.qml"):
                    if not component.stem[0].isupper():
                        continue
                    self.assertIn(component.name, exports, f"Compiled type not exported: {component}")
                    self.assertIn('"' + component.relative_to(ROOT).as_posix() + '"', fallback)


if __name__ == "__main__":
    unittest.main()
