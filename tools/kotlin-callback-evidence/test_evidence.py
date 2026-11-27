#!/usr/bin/env python3
"""Exercise the plugin with a supported trusted Kotlin compiler and JDK."""

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest
import zipfile
from unittest import mock

from build import build
import snapshot


def utf16_slice(source: str, start: int, end: int) -> str:
    return source.encode("utf-16-le")[start * 2 : end * 2].decode("utf-16-le")


class EvidenceTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.kotlinc = pathlib.Path(os.environ["AUTOREVIEW_TEST_KOTLINC"]).resolve(strict=True)
        cls.compiler_jar = pathlib.Path(os.environ["AUTOREVIEW_TEST_KOTLIN_COMPILER_JAR"]).resolve(strict=True)
        cls.version = os.environ.get("AUTOREVIEW_TEST_KOTLIN_VERSION", "2.2.20")
        cls.javac = pathlib.Path(os.environ["AUTOREVIEW_TEST_JAVAC"]).resolve(strict=True)
        cls.directory = tempfile.TemporaryDirectory(prefix="autoreview-kotlin-test-")
        cls.root = pathlib.Path(cls.directory.name)
        cls.plugin = cls.root / "evidence.jar"
        build(cls.kotlinc, cls.compiler_jar, cls.plugin, cls.version)
        (cls.root / "Auth.java").write_text("package vendor; public interface Auth { boolean accept(String s); }", encoding="utf-8")
        (cls.root / "Server.java").write_text(
            "package vendor; public class Server { public Auth getAuthenticator() { return null; } "
            "public void setAuthenticator(Auth value) {} }", encoding="utf-8"
        )
        (cls.root / "EagerServer.java").write_text(
            "package vendor; public class EagerServer extends Server { "
            "@Override public void setAuthenticator(Auth value) { value.accept(\"now\"); } }", encoding="utf-8"
        )
        cls.classes = cls.root / "classes"
        subprocess.run([str(cls.javac), "-d", str(cls.classes), str(cls.root / "Auth.java"), str(cls.root / "Server.java"), str(cls.root / "EagerServer.java")], check=True, timeout=60)

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
        self.assertEqual((report["version"], report["compiler"]), (2, self.version))
        self.assertEqual(len(report["records"]), 1, report)
        record = report["records"][0]
        self.assertEqual(record["sha256"], hashlib.sha256(source.encode()).hexdigest())
        self.assertEqual((record["callee"], record["parameter"], record["samType"]), ("vendor.Server.setAuthenticator", 0, "vendor.Auth"))
        self.assertEqual(record["parameterTypes"], ["vendor.Auth"])
        self.assertEqual(record["receiverStaticType"], "vendor.Server")
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

    def test_static_receiver_type_does_not_prove_runtime_dispatch(self) -> None:
        source = "import vendor.Auth\nimport vendor.Server\nimport vendor.EagerServer\nfun install() { val s: Server = EagerServer(); s.authenticator = Auth { true } }\n"
        result = self.compile("receiver", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = json.loads((self.root / "receiver.json").read_text(encoding="utf-8"))["records"]
        self.assertEqual(len(records), 1)
        self.assertEqual((records[0]["callee"], records[0]["modality"], records[0]["receiverStaticType"]), ("vendor.Server.setAuthenticator", "OPEN", "vendor.Server"))

    def test_receiver_property_setter_observations_include_unknown_values(self) -> None:
        source = (
            "package demo\nimport vendor.Auth\nimport vendor.Server\n"
            "fun create(): Server = Server()\n"
            "class Holder(private var server: Server) { "
            "fun install() { server.authenticator = Auth { true } }; "
            "fun change(s: Server) { server = s }; fun reset() { server = create() } }\n"
        )
        result = self.compile("property_writes", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = json.loads((self.root / "property_writes.json").read_text(encoding="utf-8"))["records"]
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["receiverProperty"], "demo.Holder.server")
        self.assertEqual(records[0]["observedSetterCallees"], [None, "demo.create"])

    def test_kotlin_local_call_resolution(self) -> None:
        source = self.root / "resolution.kt"
        source.write_text(
            'class Example { fun call(n: Int) { println("member") }; fun same() { println("member same") }; fun run() { val call = { println("lambda") }; call(1); val same = { println("local same") }; same() } }\n'
            'fun localFunction() { val action = { println("lambda") }; fun action() { println("function") }; action() }\n'
            'fun localValue() { val action = { println("lambda") }; action() }\n'
            'fun main() { localFunction(); localValue(); Example().run() }\n', encoding="utf-8"
        )
        artifact = self.root / "resolution.jar"
        subprocess.run([str(self.kotlinc), str(source), "-include-runtime", "-d", str(artifact)], check=True, capture_output=True, timeout=120)
        java = self.javac.with_name("java.exe" if os.name == "nt" else "java")
        result = subprocess.run([str(java), "-jar", str(artifact)], check=True, capture_output=True, text=True, timeout=60)
        self.assertEqual(result.stdout.splitlines(), ["function", "lambda", "member", "local same"])

    def test_failed_compilation_does_not_produce_evidence(self) -> None:
        result = self.compile("invalid", "fun broken() { missing() }\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "invalid.json").exists())

    @unittest.skipUnless(os.getenv("AUTOREVIEW_TEST_REDICLOUD_INPUTS") and os.getenv("AUTOREVIEW_TEST_REDICLOUD_BEFORE") and os.getenv("AUTOREVIEW_TEST_REDICLOUD_AFTER"), "RediCloud inputs not configured")
    def test_redicloud_historical_callback_evidence(self) -> None:
        self.assertEqual(self.version, "2.3.21")
        before = pathlib.Path(os.environ["AUTOREVIEW_TEST_REDICLOUD_BEFORE"]).resolve(strict=True)
        after = pathlib.Path(os.environ["AUTOREVIEW_TEST_REDICLOUD_AFTER"]).resolve(strict=True)
        lines = pathlib.Path(os.environ["AUTOREVIEW_TEST_REDICLOUD_INPUTS"]).read_text(encoding="utf-8").splitlines()
        sources = [pathlib.Path(line[13:]).resolve(strict=True) for line in lines if line.startswith("PROBE_SOURCE ")]
        jars = [snapshot.resolve_input(pathlib.Path(line[10:])) for line in lines if line.startswith("PROBE_JAR ")]
        self.assertEqual((len(sources), len(jars)), (7, 50))
        relative = [source.relative_to(before) for source in sources]
        target = pathlib.Path("file-cluster/src/main/kotlin/dev/redicloud/cluster/file/FileCluster.kt")
        self.assertIn(target, relative)
        hashes = ["e731be100e841140a07e0830bd28fca14c804d76bd6a008c14ad7285ec267a7c", "e5c2238317c40c8a9c823d6c2a09714ce1b3081234e40ae17713a6e5536fc7e7"]
        reports = []
        for index, root in enumerate((before, after)):
            files = [root / path for path in relative]
            self.assertEqual(hashlib.sha256((root / target).read_bytes()).hexdigest(), hashes[index])
            for path in relative:
                if path != target:
                    self.assertEqual((before / path).read_bytes(), (root / path).read_bytes())
            output = self.root / f"redicloud-{index}.json"
            snapshot.run(self.kotlinc, self.plugin, files, jars, output, jvm_target="21")
            reports.append(json.loads(output.read_text(encoding="utf-8")))
        self.assertEqual(reports[0]["dependencies"], reports[1]["dependencies"])
        for report in reports:
            records = [record for record in report["evidence"]["records"] if record["callee"] == "org.apache.sshd.server.SshServer.setPasswordAuthenticator"]
            self.assertEqual(len(records), 1)
            self.assertEqual((records[0]["samType"], records[0]["parameter"], records[0]["modality"]), ("org.apache.sshd.server.auth.password.PasswordAuthenticator", 0, "OPEN"))
            self.assertEqual(records[0]["receiverStaticType"], "org.apache.sshd.server.SshServer")
            self.assertEqual(records[0]["receiverProperty"], "dev.redicloud.cluster.file.FileCluster.sshd")
            self.assertEqual(records[0]["observedSetterCallees"], ["org.apache.sshd.server.SshServer.setUpDefaultServer"])

    def test_callback_parameter_index(self) -> None:
        source = "package demo\nimport vendor.Auth\nfun register(n: Int, callback: Auth) {}\nfun install() { register(1, Auth { true }) }\n"
        result = self.compile("parameter", source)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.root / "parameter.json").read_text(encoding="utf-8"))
        self.assertEqual([(record["callee"], record["parameter"], record["samType"]) for record in report["records"]], [("demo.register", 1, "vendor.Auth")])
        self.assertEqual(report["records"][0]["parameterTypes"], ["kotlin.Int", "vendor.Auth"])
        self.assertIsNone(report["records"][0]["receiverStaticType"])

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

    def snapshot_dependencies(self) -> list[pathlib.Path]:
        fixture = self.root / "fixture data.jar"
        if not fixture.exists():
            with zipfile.ZipFile(fixture, "w") as archive:
                for entry in self.classes.rglob("*.class"):
                    archive.write(entry, entry.relative_to(self.classes).as_posix())
        return [fixture, self.compiler_jar.parent / "kotlin-stdlib.jar"]

    def test_snapshot_compiles_copied_inputs_and_maps_source(self) -> None:
        source = self.root / "snapshot name.kt"
        text = "package demo\nimport vendor.Auth\nimport vendor.Server\nfun install(s: Server) { s.authenticator = Auth { true } }\n"
        source.write_text(text, encoding="utf-8", newline="\n")
        output = self.root / "snapshot-report.json"
        snapshot.run(self.kotlinc, self.plugin, [source], self.snapshot_dependencies(), output, "21")
        report = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(report["snapshotVersion"], 1)
        self.assertEqual(report["jvmTarget"], "21")
        self.assertEqual(report["sources"][0], {"path": str(source.resolve()), "sha256": hashlib.sha256(text.encode()).hexdigest()})
        self.assertEqual(report["evidence"]["records"][0]["path"], str(source.resolve()))
        self.assertEqual(report["evidence"]["records"][0]["callee"], "vendor.Server.setAuthenticator")
        self.assertEqual(len(report["dependencies"]), 2)
        with self.assertRaises(FileExistsError):
            snapshot.run(self.kotlinc, self.plugin, [source], self.snapshot_dependencies(), output)
        with self.assertRaisesRegex(ValueError, "JVM target"):
            snapshot.run(self.kotlinc, self.plugin, [source], self.snapshot_dependencies(), self.root / "invalid-target.json", "-Xplugin=unexpected")

    @unittest.skipUnless(os.name == "nt", "Windows extended paths")
    def test_snapshot_reads_long_windows_source_path(self) -> None:
        long_path = self.root.joinpath(*(["long-directory-" + "x" * 45] * 4), "Long.kt")
        extended = pathlib.Path("\\\\?\\" + str(long_path))
        long_jar = long_path.with_name("fixture.jar")
        extended_jar = extended.with_name("fixture.jar")
        extended.parent.mkdir(parents=True)
        try:
            extended.write_text("fun valid() {}\n", encoding="utf-8", newline="\n")
            shutil.copyfile(self.snapshot_dependencies()[0], extended_jar)
            output = self.root / "long-path-report.json"
            snapshot.run(self.kotlinc, self.plugin, [long_path], [long_jar, self.snapshot_dependencies()[1]], output)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(report["sources"][0]["sha256"], hashlib.sha256(b"fun valid() {}\n").hexdigest())
        finally:
            extended.unlink(missing_ok=True)
            extended_jar.unlink(missing_ok=True)
            parent = extended.parent
            for _ in range(4):
                parent.rmdir()
                parent = parent.parent

    def test_snapshot_rejects_source_changes_and_compile_failure(self) -> None:
        source = self.root / "changed.kt"
        source.write_text("fun valid() {}\n", encoding="utf-8")
        output = self.root / "changed-report.json"
        original_run = subprocess.run
        def change_source(*args, **kwargs):
            result = original_run(*args, **kwargs)
            source.write_text("fun changed() {}\n", encoding="utf-8")
            return result
        with mock.patch.object(snapshot.subprocess, "run", side_effect=change_source):
            with self.assertRaisesRegex(ValueError, "changed"):
                snapshot.run(self.kotlinc, self.plugin, [source], self.snapshot_dependencies(), output)
        self.assertFalse(output.exists())
        source.write_text("fun invalid() { missing() }\n", encoding="utf-8")
        with self.assertRaises(subprocess.CalledProcessError):
            snapshot.run(self.kotlinc, self.plugin, [source], self.snapshot_dependencies(), output)
        self.assertFalse(output.exists())

    def test_snapshot_rejects_changed_dependency(self) -> None:
        source = self.root / "dependency.kt"
        source.write_text("fun valid() {}\n", encoding="utf-8")
        output = self.root / "dependency-report.json"
        jar = self.root / "changing.jar"
        shutil.copyfile(self.snapshot_dependencies()[0], jar)
        original_run = subprocess.run
        def change_jar(*args, **kwargs):
            result = original_run(*args, **kwargs)
            with jar.open("ab") as destination:
                destination.write(b"changed")
            return result
        with mock.patch.object(snapshot.subprocess, "run", side_effect=change_jar):
            with self.assertRaisesRegex(ValueError, "changed"):
                snapshot.run(self.kotlinc, self.plugin, [source], [jar, self.snapshot_dependencies()[1]], output)
        self.assertFalse(output.exists())

    def test_snapshot_rejects_invalid_evidence_spans(self) -> None:
        source = self.root / "span.kt"
        source.write_text("fun x() {}\n", encoding="utf-8")
        copied = self.root / "copied.kt"
        shutil.copyfile(source, copied)
        item = {"original": source, "snapshot": copied, "sha256": hashlib.sha256(source.read_bytes()).hexdigest(), "limit": snapshot.SOURCE_LIMIT}
        output = self.root / "bad-span.json"
        record = {"path": str(copied), "sha256": item["sha256"], "callStartUtf16": 0,
                  "lambdaStartUtf16": 1, "lambdaEndUtf16": 2, "callEndUtf16": 999}
        output.write_text(json.dumps({"version": 2, "compiler": "2.2.20", "records": [record]}), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "span"):
            snapshot.verified_report(output, [item], [item])


if __name__ == "__main__":
    unittest.main()
