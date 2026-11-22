use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde_yaml::{Mapping, Value};

const FAMILY_SOURCES: &[&str] = &["rule-families/loops.yml", "rule-families/extended.yml"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=rules-builtin");

    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("compiled-rules");
    match fs::remove_dir_all(&output) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    copy_rule_tree(Path::new("rules-builtin"), &output)?;
    for source in FAMILY_SOURCES {
        println!("cargo:rerun-if-changed={source}");
        compile_families(source, &output)?;
    }
    Ok(())
}

fn copy_rule_tree(source: &Path, output: &Path) -> io::Result<()> {
    fs::create_dir_all(output)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let dest = output.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_rule_tree(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn compile_families(source: &str, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut families: Value = serde_yaml::from_str(&fs::read_to_string(source)?)?;
    families.apply_merge()?;
    let families = families.as_mapping().expect("rule families must be a YAML mapping");
    for (name, family) in families {
        let name = name.as_str().expect("family name must be a string");
        let mut common = family.as_mapping().expect("family must be a mapping").clone();
        let languages = common.remove("languages").expect("family requires languages");
        for (language, overrides) in languages.as_mapping().expect("languages must be a mapping") {
            let language = language.as_str().expect("language must be a string");
            write_variant(output, name, language, &common, overrides)?;
        }
    }
    Ok(())
}

fn write_variant(
    output: &Path,
    name: &str,
    language: &str,
    common: &Mapping,
    overrides: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rule = common.clone();
    rule.extend(overrides.as_mapping().expect("rule variant must be a mapping").clone());
    let directory = match language {
        "Go" => "go",
        "Java" => "java",
        "Kotlin" => "kotlin",
        "JavaScript" => "javascript",
        "TypeScript" => "typescript",
        "Tsx" => "typescript",
        "Rust" => "rust",
        _ => return Err(format!("unsupported language {language} in {name}").into()),
    };
    let id = format!("{}-{name}", language.to_ascii_lowercase());
    rule.insert("id".into(), id.into());
    rule.insert("language".into(), language.into());
    let category = rule.get("category").and_then(Value::as_str).expect("rule variant requires a category");
    assert!(safe_component(name) && safe_component(category), "invalid rule path for {name}");
    let filename = if language == "Tsx" { format!("tsx-{name}") } else { name.into() };
    let output = output.join(directory).join(category).join(format!("{filename}.yml"));
    fs::create_dir_all(output.parent().expect("rule output has a directory"))?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&output)?;
    file.write_all(serde_yaml::to_string(&rule)?.as_bytes())?;
    Ok(())
}

fn safe_component(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
