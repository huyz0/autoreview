use std::collections::BTreeMap;

use autoreview_schema::OutboxOperationConfig;

use super::super::web::imported_operations::FileCalls;
use super::Node;

pub(super) struct Endpoints<'tree> {
    pub write: BTreeMap<usize, Node<'tree>>,
    pub publish: BTreeMap<usize, Node<'tree>>,
}

impl Endpoints<'_> {
    pub fn has_both_in(&self, function: Node<'_>) -> bool {
        let range = function.byte_range();
        self.write.range(range.clone()).next().is_some()
            && self.publish.range(range).next().is_some()
    }
}

pub(super) fn resolve<'tree>(
    calls: &FileCalls<'tree, '_>,
    operation: &OutboxOperationConfig,
) -> Option<Endpoints<'tree>> {
    let write = calls.matching_calls(&operation.database_write);
    let mut publish = calls.matching_calls(&operation.broker_publish);
    // One invocation cannot serve as evidence of two different operations.
    publish.retain(|start, _| !write.contains_key(start));
    (!write.is_empty() && !publish.is_empty()).then_some(Endpoints { write, publish })
}
