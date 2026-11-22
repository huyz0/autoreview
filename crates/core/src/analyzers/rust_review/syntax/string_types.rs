use tree_sitter::Node;

use super::Source;

impl<'tree> Source<'tree, '_> {
    pub(in crate::analyzers) fn is_standard_string(&self, mut ty: Node<'tree>) -> bool {
        while ty.kind() == "reference_type" {
            let Some(inner) = ty.child_by_field_name("type") else {
                return false;
            };
            ty = inner;
        }
        ty.kind() == "primitive_type" && self.text(ty) == "str"
            || self.unshadowed_prelude(ty, "String")
            || matches!(
                self.external_path(ty).as_deref(),
                Some("std::string::String" | "alloc::string::String")
            )
    }

    pub(in crate::analyzers) fn constructs_string(&self, call: Node<'tree>) -> bool {
        let Some(function) = call.child_by_field_name("function") else {
            return false;
        };
        let Some(owner) = function.child_by_field_name("path") else {
            return false;
        };
        function
            .child_by_field_name("name")
            .is_some_and(|name| matches!(self.text(name), "from" | "new" | "with_capacity"))
            && self.is_standard_string(owner)
    }
}
