#!/usr/bin/env python3
"""Build the optional Kotlin compiler plugin using an explicitly supplied compiler."""

import argparse
import pathlib
import subprocess
import tempfile
import zipfile


def build(kotlinc: pathlib.Path, compiler_jar: pathlib.Path, output: pathlib.Path, version: str = "2.2.20") -> None:
    root = pathlib.Path(__file__).resolve().parent
    source = root / "src" / "EvidencePlugin.kt"
    if version not in ("2.2.20", "2.3.21"):
        raise ValueError("unsupported Kotlin compiler version")
    registrar = root / "src" / f"Registrar-{version}.kt"
    service = root / "src" / "META-INF" / "services" / "org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar"
    if output.exists():
        raise FileExistsError(output)
    with tempfile.TemporaryDirectory(prefix="autoreview-kotlin-plugin-") as directory:
        classes = pathlib.Path(directory) / "classes"
        subprocess.run(
            [str(kotlinc), str(source), str(registrar), "-cp", str(compiler_jar), "-d", str(classes)],
            check=True,
            timeout=120,
        )
        with output.open("xb") as artifact, zipfile.ZipFile(artifact, "w", zipfile.ZIP_DEFLATED) as archive:
            for item in sorted(classes.rglob("*")):
                if item.is_file():
                    archive.write(item, item.relative_to(classes).as_posix())
            archive.write(service, "META-INF/services/org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kotlinc", type=pathlib.Path, required=True)
    parser.add_argument("--compiler-jar", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--compiler-version", choices=("2.2.20", "2.3.21"), default="2.2.20")
    args = parser.parse_args()
    build(args.kotlinc.resolve(strict=True), args.compiler_jar.resolve(strict=True), args.output.absolute(), args.compiler_version)


if __name__ == "__main__":
    main()
