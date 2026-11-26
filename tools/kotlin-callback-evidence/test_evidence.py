#!/usr/bin/env python3
"""Exercise the plugin with a trusted Kotlin 2.2.20 compiler and JDK."""

import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import unittest

from build import build


def utf16_slice(source: str, start: int, end: int) -> str:
    return source.encode("utf-16-le")[start * 2 : end * 2].decode("utf-16-le")


class EvidenceTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.kotlinc = pathlib.Path(os.environ["AUTOREVIEW_TEST_KOTLINC"]).resolve(strict=True)
        cls.compiler_jar = pathlib.Path(os.environ["AUTOREVIEW_TEST_KOTLIN_COMPILER_JAR"]).resolve(strict=True)
        cls.javac = pathlib.Path(os.environ["AUTOREVIEW_TEST_JAVAC"]).resolve(strict=True)
        cls.directory = tempfile.TemporaryDirectory(prefix="autoreview-kotlin-test-")
        cls.root = pathlib.Path(cls.directory.name)
        cls.plugin = cls.root / "evidence.jar"
        build(cls.kotlinc, cls.compiler_jar, cls.plugin)
        (cls.root / "Auth.java").write_text("package vendor; public interface Auth { boolean accept(String s); }", encoding="utf-8")
        (cls.root / "Server.java").write_text(
            "package vendor; public class Server { public Auth getAuthenticator() { return null; } "
            "public void setAuthenticator(Auth value) {} }", encoding="utf-8"
        )
        cls.classes = cls.root / "classes"
        subprocess.run([str(cls.javac), "-d", str(cls.classes), str(cls.root / "Auth.java"), str(cls.root / "Server.java")], check=True, timeout=60)

    @classmethod
    def tearDownClass(cls) -> None:
        cls.directory.cleanup()

    def compile(self, name: str, source: str, output: pathlib.Path | None = None) -> subprocess.CompletedProcess:
        file = self.root / f"{name}.kt"
        file.write_text(source, encoding="utf-8", newline="\n")
        destination = output or self.root / f"{name}.json"
        env = os.environ.copy()
        env["AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT"] = str(destination)
        return subprocess.run(
            [str(self.kotlinc), str(file), "-cp", str(self.classes), "-d", str(self.root / f"{name}-out"), f"-Xplugin={self.plugin}"],
            env=env, capture_output=True, text=True, timeout=120,
        )

    def test_java_sam_and_utf16_offsets(self) -> None:
        source = "package demo\nimport vendor.Auth\nimport vendor.Server\n// 😀\nsuspend fun install(s: Server) { s.authenticator = Auth { value -> value.isNotEmpty() } }\n"
        result = self.compile("sample", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.root / "sample.json").read_text(encoding="utf-8"))
        self.assertEqual((report["version"], report["compiler"]), (1, "2.2.20"))
        self.assertEqual(len(report["records"]), 1, report)
        record = report["records"][0]
        self.assertEqual(record["sha256"], hashlib.sha256(source.encode()).hexdigest())
        self.assertEqual((record["callee"], record["parameter"], record["samType"]), ("vendor.Server.setAuthenticator", 0, "vendor.Auth"))
        self.assertEqual(utf16_slice(source, record["lambdaStartUtf16"], record["lambdaEndUtf16"]), "{ value -> value.isNotEmpty() }")
        self.assertIn("authenticator =", utf16_slice(source, record["callStartUtf16"], record["callEndUtf16"]))

    def test_same_name_function_is_not_sam_evidence(self) -> None:
        source = "package demo\nimport vendor.Auth\nimport vendor.Server\nfun Auth(f: () -> Boolean): Auth = vendor.Auth { f() }\nfun install(s: Server) { s.authenticator = Auth(f = { true }) }\n"
        result = self.compile("shadowed", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.root / "shadowed.json").read_text(encoding="utf-8"))
        # The function body legitimately constructs a SAM; the outer call must
        # not be mistaken for a SAM conversion at the setter.
        self.assertFalse(any(record["callee"] == "vendor.Server.setAuthenticator" for record in report["records"]))

    def test_failed_compilation_does_not_produce_evidence(self) -> None:
        result = self.compile("invalid", "fun broken() { missing() }\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "invalid.json").exists())

    def test_callback_parameter_index(self) -> None:
        source = "package demo\nimport vendor.Auth\nfun register(n: Int, callback: Auth) {}\nfun install() { register(1, Auth { true }) }\n"
        result = self.compile("parameter", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.root / "parameter.json").read_text(encoding="utf-8"))
        self.assertEqual([(record["callee"], record["parameter"], record["samType"]) for record in report["records"]], [("demo.register", 1, "vendor.Auth")])

    def test_existing_output_is_not_overwritten(self) -> None:
        output = self.root / "existing.json"
        output.write_text("sentinel", encoding="utf-8")
        result = self.compile("existing", "fun valid() {}\n", output)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(output.read_text(encoding="utf-8"), "sentinel")


if __name__ == "__main__":
    unittest.main()
