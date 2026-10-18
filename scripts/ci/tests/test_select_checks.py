import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "select_checks", Path(__file__).resolve().parents[1] / "select_checks.py")
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)


class SelectChecksTests(unittest.TestCase):
    def test_documentation_only(self):
        plan = ci.select_checks(["README.md", "spec/dev/quality/tests_and_benchmarks.md"])
        self.assertFalse(plan["rust"])
        self.assertFalse(plan["ui"])

    def test_tui_selects_only_itself(self):
        plan = ci.select_checks(["crates/akasha-tui/src/main.rs"])
        self.assertEqual(plan["packages"], ["akasha-tui"])
        self.assertFalse(plan["full"])
        self.assertFalse(plan["ui"])

    def test_ui_only(self):
        plan = ci.select_checks(["apps/akasha-ui/src/App.tsx"])
        self.assertTrue(plan["ui"])
        self.assertFalse(plan["rust"])

    def test_transverse_files_and_unknown_paths(self):
        for path in ["Cargo.lock", "crates/akasha-tui/Cargo.toml",
                     "apps/akasha-ui/src-tauri/Cargo.lock",
                     "apps/akasha-ui/src-tauri/.cargo/config.toml",
                     "apps/akasha-ui/rust-toolchain.toml", ".github/workflows/ci.yml",
                     "scripts/install.ps1", "vendor/esaxx-rs/build.rs",
                     "spec/embedded_models.json", "embedding_model/config.json",
                     "crates/deleted-crate/src/lib.rs"]:
            with self.subTest(path=path):
                self.assertTrue(ci.select_checks([path])["full"])

    def test_runtime_documentation_is_not_exempt(self):
        self.assertTrue(ci.select_checks(["docs/user/accueil.md"])["ui"])

    def test_core_and_reverse_daemon_dependencies_select_full(self):
        for package in ["akasha-core", "akasha-llm", "akasha-store", "akasha-vault"]:
            with self.subTest(package=package):
                self.assertTrue(ci.select_checks([f"crates/{package}/src/lib.rs"])["full"])

    def test_mixed_and_windows_paths(self):
        plan = ci.select_checks(["README.md", r"crates\akasha-tui\src\main.rs",
                                 "apps/akasha-ui/src/App.tsx"])
        self.assertEqual(plan["packages"], ["akasha-tui"])
        self.assertTrue(plan["ui"])

    def test_force_full_and_missing_graph(self):
        self.assertTrue(ci.select_checks([], force_full=True)["full"])
        with tempfile.TemporaryDirectory() as directory:
            self.assertTrue(ci.select_checks([], Path(directory))["full"])

    def test_git_filename_cannot_inject_github_outputs(self):
        plan = ci.select_checks(["unknown\nrust=false\nui=false"])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "outputs"
            ci.write_outputs(path, plan)
            outputs = dict(line.split("=", 1) for line in path.read_text().splitlines())
            self.assertEqual(outputs["rust"], "true")
            self.assertEqual(outputs["ui"], "true")
            self.assertEqual(len(outputs), len(plan))

    def test_reverse_optional_and_dev_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[workspace]\nmembers=["a","b","c"]\n')
            for name, deps in [
                ("a", ""), ("b", '[dependencies]\na={path="../a", optional=true}\n'),
                ("c", '[dev-dependencies]\nb={path="../b"}\n'),
            ]:
                (root / name).mkdir()
                (root / name / "Cargo.toml").write_text(
                    f'[package]\nname="{name}"\nversion="0.1.0"\n' + deps)
            self.assertEqual(ci.select_checks(["a/src/lib.rs"], root)["packages"], ["a", "b", "c"])

    def test_git_moves_include_both_sides(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.STDOUT)
            git("init")
            git("config", "user.name", "CI test")
            git("config", "user.email", "ci@example.invalid")
            (root / "before.rs").write_text("source")
            git("add", ".")
            git("commit", "-m", "before")
            base = git("rev-parse", "HEAD").decode().strip()
            git("mv", "before.rs", "README.md")
            git("commit", "-m", "move")
            self.assertEqual(set(ci.changed_paths(base, "HEAD", root)), {"before.rs", "README.md"})


if __name__ == "__main__":
    unittest.main()
