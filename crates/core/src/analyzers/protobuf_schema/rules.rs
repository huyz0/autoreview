use super::Rule;

const POLICY: &str = "artifactChecks.protobufCompatibility";
const SCHEMA_GUIDE: &str = "https://protobuf.dev/programming-guides/proto3/#updating";
const TOOL: &str = "autoreview-protobuf";

pub(in crate::analyzers) const REUSED_TAG: Rule = Rule {
    id: "protobuf-field-tag-reused",
    category: "correctness",
    subcategory: "compatibility.schema-evolution",
    tool: TOOL,
    title: "Protobuf field number assigned to another field",
    reference: SCHEMA_GUIDE,
    enabled_by: POLICY,
};

pub(in crate::analyzers) const UNRESERVED_TAG: Rule = Rule {
    id: "protobuf-removed-field-tag-not-reserved",
    category: "correctness",
    subcategory: "compatibility.schema-evolution",
    tool: TOOL,
    title: "Removed Protobuf field number is still available",
    reference: SCHEMA_GUIDE,
    enabled_by: POLICY,
};

pub(in crate::analyzers) const INCOMPATIBLE_FIELD: Rule = Rule {
    id: "protobuf-incompatible-field-change",
    category: "correctness",
    subcategory: "compatibility.rolling-upgrades",
    tool: TOOL,
    title: "Protobuf field change can break mixed-version readers",
    reference: SCHEMA_GUIDE,
    enabled_by: POLICY,
};
