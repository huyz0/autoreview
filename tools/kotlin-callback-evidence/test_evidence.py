#!/usr/bin/env python3
"""Exercise the plugin with a trusted Kotlin 2.2.20 compiler and JDK."""

import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import unittest
import zipfile

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

    def compile(self, name: str, source: str, output: pathlib.Path | None = None, classpath: list[pathlib.Path] | None = None) -> subprocess.CompletedProcess:
        file = self.root / f"{name}.kt"
        file.write_text(source, encoding="utf-8", newline="\n")
        destination = output or self.root / f"{name}.json"
        env = os.environ.copy()
        env["AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT"] = str(destination)
        classpath_arg = os.pathsep.join(str(path) for path in (classpath or [self.classes]))
        return subprocess.run(
            [str(self.kotlinc), str(file), "-cp", classpath_arg, "-d", str(self.root / f"{name}-out"), f"-Xplugin={self.plugin}"],
            env=env, capture_output=True, text=True, timeout=120,
        )

    def test_java_sam_and_utf16_offsets(self) -> None:
        source = "package demo\nimport vendor.Auth\nimport vendor.Server\n// 😀\nsuspend fun install(s: Server) { s.authenticator = Auth { value -> value.isNotEmpty() } }\n"
        result = self.compile("sample", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.root / "sample.json").read_text(encoding="utf-8"))
        self.assertEqual((report["version"], report["compiler"]), (2, "2.2.20"))
        self.assertEqual(len(report["records"]), 1, report)
        record = report["records"][0]
        self.assertEqual(record["sha256"], hashlib.sha256(source.encode()).hexdigest())
        self.assertEqual((record["callee"], record["parameter"], record["samType"]), ("vendor.Server.setAuthenticator", 0, "vendor.Auth"))
        self.assertEqual(record["parameterTypes"], ["vendor.Auth"])
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
        self.assertEqual(report["records"][0]["parameterTypes"], ["kotlin.Int", "vendor.Auth"])

    def test_overloaded_calls_have_distinct_parameter_types(self) -> None:
        source = "package demo\nimport vendor.Auth\nfun register(n: Int, callback: Auth) {}\nfun register(n: String, callback: Auth) {}\nfun install() { register(1, Auth { true }); register(\"a\", Auth { true }) }\n"
        result = self.compile("overloads", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = json.loads((self.root / "overloads.json").read_text(encoding="utf-8"))["records"]
        self.assertEqual([record["parameterTypes"] for record in records], [["kotlin.Int", "vendor.Auth"], ["kotlin.String", "vendor.Auth"]])

    def test_generic_parameter_is_unknown(self) -> None:
        source = "package demo\nimport vendor.Auth\nfun <T> register(n: T, callback: Auth) {}\nfun install() { register(1, Auth { true }) }\n"
        result = self.compile("generic", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = json.loads((self.root / "generic.json").read_text(encoding="utf-8"))["records"]
        self.assertEqual(records[0]["parameterTypes"], [None, "vendor.Auth"])

    @unittest.skipUnless(os.getenv("AUTOREVIEW_TEST_SSHD_CORE") and os.getenv("AUTOREVIEW_TEST_SSHD_COMMON"), "Apache SSHD artifacts not configured")
    def test_apache_sshd_password_authenticator(self) -> None:
        source = "package demo\nimport org.apache.sshd.server.SshServer\nimport org.apache.sshd.server.auth.password.PasswordAuthenticator\nsuspend fun install(s: SshServer) { s.passwordAuthenticator = PasswordAuthenticator { _, _, _ -> true } }\n"
        jars = [pathlib.Path(os.environ[name]).resolve(strict=True) for name in ("AUTOREVIEW_TEST_SSHD_CORE", "AUTOREVIEW_TEST_SSHD_COMMON")]
        # A single classpath directory avoids kotlinc.bat splitting a Windows
        # semicolon-delimited classpath. Only trusted, pinned test jars are read.
        classes = self.root / "sshd-classes"
        for jar in jars:
            with zipfile.ZipFile(jar) as archive:
                for entry in archive.infolist():
                    path = pathlib.PurePosixPath(entry.filename)
                    if path.is_absolute() or ".." in path.parts or "\\" in entry.filename or ":" in entry.filename or not entry.filename.endswith(".class"):
                        continue
                    target = classes.joinpath(*path.parts)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(archive.read(entry))
        result = self.compile("sshd", source, classpath=[classes])
        self.assertEqual(result.returncode, 0, result.stderr)
        records = json.loads((self.root / "sshd.json").read_text(encoding="utf-8"))["records"]
        self.assertEqual(len(records), 1, records)
        record = records[0]
        self.assertEqual(record["callee"], "org.apache.sshd.server.SshServer.setPasswordAuthenticator")
        self.assertEqual(record["parameterTypes"], ["org.apache.sshd.server.auth.password.PasswordAuthenticator"])
        self.assertEqual(record["samType"], "org.apache.sshd.server.auth.password.PasswordAuthenticator")
        self.assertEqual(record["modality"], "OPEN")

    def test_existing_output_is_not_overwritten(self) -> None:
        output = self.root / "existing.json"
        output.write_text("sentinel", encoding="utf-8")
        result = self.compile("existing", "fun valid() {}\n", output)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(output.read_text(encoding="utf-8"), "sentinel")


if __name__ == "__main__":
    unittest.main()
