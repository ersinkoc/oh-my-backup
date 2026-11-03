import pathlib
import subprocess
import sys
import tempfile
import unittest

CHECKER = pathlib.Path(__file__).with_name("check-source-privacy.py").resolve()


class PrivacyGuardTests(unittest.TestCase):
    def check(self, files):
        with tempfile.TemporaryDirectory(prefix="oh-my-backup-privacy-") as directory:
            subprocess.run(["git", "init", "-q", directory], check=True)
            for name, content in files.items():
                path = pathlib.Path(directory, name)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
            if files:
                subprocess.run(["git", "add", "."], cwd=directory, check=True, capture_output=True)
            return subprocess.run([sys.executable, str(CHECKER), "--staged"], cwd=directory, capture_output=True, text=True)

    def test_empty_index_does_not_claim_a_pass(self):
        result = self.check({})
        self.assertEqual(result.returncode, 1)
        self.assertIn("no tracked files", result.stdout)

    def test_generic_source_and_sdk_versions_pass(self):
        result = self.check({"src/main.rs": 'const HOST: &str = "backup.example.invalid";', "Cargo.lock": 'version = "0.4.0+sdk-1.4.341.0"'})
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("2 staged files", result.stdout)

    def test_saved_config_is_rejected(self):
        result = self.check({"config.json": "{}"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("must not be tracked", result.stdout)

    def test_credentials_and_server_addresses_are_redacted(self):
        token = "gh" + "p_" + "X" * 36
        address = "192" + ".0.2.1"
        result = self.check({"fixture.txt": token + " " + address})
        self.assertEqual(result.returncode, 1)
        self.assertIn("GitHub credential", result.stdout)
        self.assertIn("IPv4 address", result.stdout)
        self.assertNotIn(token, result.stdout)
        self.assertNotIn(address, result.stdout)


if __name__ == "__main__":
    unittest.main()
