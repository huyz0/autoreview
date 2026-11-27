//! Contract for the audited Apache SSHD 2.17.1 implementation.
use serde::Deserialize;
use super::Record;

pub(super) const JAR: &str = "b30769bbc053da28949ed1404abed546598ccf3de9100945b627f5b3a0fe003c";
pub(super) const CLASSES: &[(&str, &str)] = &[
    ("org/apache/sshd/server/SshServer.class", "ad0d0841d3a9f34db35893bf0a822cb16ef9e38c1021c81a0163f9209fe38a63"),
    ("org/apache/sshd/server/ServerBuilder.class", "8a07ba534e3d3076bc5d937e2bf10ebe7034bab8fd4e8c72ff6f59740fe569b0"),
    ("org/apache/sshd/common/BaseBuilder.class", "0eec26bfb1e7c8660d7cd2c91ce8c067db397bbc79cb2ba396d0eff49357658c"),
    ("org/apache/sshd/server/auth/password/PasswordAuthenticator.class", "9ca914e3ad15c223fd06ab080bfcd1aa3cb1149fbced7fa8ef354d07f2aaf00b"),
];
const SERVER: &str = "org.apache.sshd.server.SshServer";
const AUTH: &str = "org.apache.sshd.server.auth.password.PasswordAuthenticator";
const FACTORY: &str = "org.apache.sshd.server.SshServer.setUpDefaultServer";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResolvedClass { name: String, jar_sha256: String, class_sha256: String }

pub(super) fn classes_match(classes: &[ResolvedClass], sources: Option<&[String]>) -> bool {
    let Some(sources) = sources else { return false };
    if sources.len() > 20_000 || sources.iter().any(|name| name.len() > 1024) { return false; }
    CLASSES.iter().all(|(path, hash)| {
        let source_name = path.trim_end_matches(".class").replace('/', ".");
        !sources.contains(&source_name) && classes.iter().filter(|class| class.name == *path).count() == 1
            && classes.iter().any(|class| class.name == *path && class.jar_sha256 == JAR && class.class_sha256 == *hash)
    })
}

pub(super) fn deferred(record: &Record) -> bool {
    call_matches(record) && property_matches(record) && factory_matches(record)
}

fn call_matches(record: &Record) -> bool {
    record.callee == format!("{SERVER}.setPasswordAuthenticator") && record.parameter == 0 && record.sam_type == AUTH
        && record.modality == "OPEN" && record.receiver_static_type.as_deref() == Some(SERVER)
        && record.receiver_property.as_ref().is_some_and(|name| !name.is_empty())
        && record.property_source_sha256.as_deref() == Some(record.sha256.as_str())
        && record.parameter_types.as_deref() == Some(&[Some(AUTH.to_owned())])
}

fn property_matches(record: &Record) -> bool {
    let Some(property) = &record.property_evidence else { return false };
    property.visibility == "private" && property.default_getter && property.default_setter && property.null_initializer
        && property.direct_field_writes == 0 && property.accessor_references == 0
}

fn factory_matches(record: &Record) -> bool {
    let Some([Some(detail)]) = record.observed_setter_details.as_deref() else { return false };
    detail.callee == FACTORY && detail.origin == "IR_EXTERNAL_JAVA_DECLARATION_STUB" && !detail.dispatch_receiver
        && detail.parameter_types.is_empty() && detail.return_type.as_deref() == Some(SERVER) && detail.type_parameters == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> serde_json::Value {
        serde_json::json!({"path":"sample.kt", "sha256":"a".repeat(64), "lambdaStartUtf16":1, "lambdaEndUtf16":2,
            "callee":format!("{SERVER}.setPasswordAuthenticator"), "parameter":0, "samType":AUTH, "modality":"OPEN",
            "parameterTypes":[AUTH], "receiverStaticType":SERVER, "receiverProperty":"example.Owner.server", "propertySourceSha256":"a".repeat(64),
            "observedSetterCallees":[FACTORY], "observedSetterDetails":[{"callee":FACTORY,"origin":"IR_EXTERNAL_JAVA_DECLARATION_STUB", "dispatchReceiver":false,
                "parameterTypes":[], "returnType":SERVER,"typeParameters":0}],
            "propertyEvidence":{"visibility":"private","defaultGetter":true,"defaultSetter":true,"nullInitializer":true,"directFieldWrites":0,"accessorReferences":0}})
    }

    #[test]
    fn class_contract_rejects_missing_duplicates_and_source_shadows() {
        let mut classes: Vec<_> = CLASSES.iter().map(|(name, hash)| ResolvedClass { name: name.to_string(), jar_sha256: JAR.into(), class_sha256: hash.to_string() }).collect();
        assert!(classes_match(&classes, Some(&[])));
        assert!(!classes_match(&classes, None));
        assert!(!classes_match(&classes, Some(&[SERVER.into()])));
        classes[0].jar_sha256 = "0".repeat(64);
        assert!(!classes_match(&classes, Some(&[])));
        classes[0].jar_sha256 = JAR.into();
        classes.push(ResolvedClass { name: CLASSES[0].0.into(), jar_sha256: JAR.into(), class_sha256: CLASSES[0].1.into() });
        assert!(!classes_match(&classes, Some(&[])));
    }

    #[test]
    fn receiver_contract_rejects_ambiguous_property_and_dispatch() {
        let good = record();
        assert!(deferred(&serde_json::from_value(good.clone()).unwrap()));
        for (pointer, value) in [
            ("/receiverStaticType", serde_json::json!("example.Subclass")),
            ("/propertyEvidence/defaultSetter", serde_json::json!(false)),
            ("/propertyEvidence/directFieldWrites", serde_json::json!(1)),
            ("/propertyEvidence/accessorReferences", serde_json::json!(1)),
            ("/propertySourceSha256", serde_json::json!("b".repeat(64))),
            ("/observedSetterDetails/0/dispatchReceiver", serde_json::json!(true)),
            ("/observedSetterDetails/0/origin", serde_json::json!("DEFINED")),
            ("/parameterTypes/0", serde_json::json!("example.Other")),
        ] {
            let mut value_json = good.clone();
            *value_json.pointer_mut(pointer).unwrap() = value;
            assert!(!deferred(&serde_json::from_value(value_json).unwrap()), "{pointer}");
        }
    }
}
