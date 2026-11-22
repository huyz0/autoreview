use std::collections::HashSet;

use super::{children, descendants, field_text, member_call, text, Node};

#[derive(Default)]
struct Open {
    falls_through: bool,
    exits: bool,
}

impl Open {
    fn branch(self, other: Self) -> Self {
        Self { falls_through: self.falls_through || other.falls_through, exits: self.exits || other.exits }
    }

    fn remains(self) -> bool {
        self.falls_through || self.exits
    }
}

pub(super) fn open_exit(steps: &[Node<'_>], binding: &str, jest: &str, source: &str) -> Option<bool> {
    let check = Check { jest, source };
    let aliases = HashSet::from([binding]);
    Some(check.block(steps.iter().copied(), aliases)?.remains())
}

pub(super) fn const_binding<'s, 't>(node: Node<'t>, source: &'s str) -> Option<(&'s str, Node<'t>)> {
    if node.kind() != "lexical_declaration" || field_text(node, "kind", source) != Some("const")
        || node.named_child_count() != 1
    {
        return None;
    }
    let declarator = node.named_child(0)?;
    let name = declarator.child_by_field_name("name").filter(|name| name.kind() == "identifier")?;
    Some((text(name, source), declarator.child_by_field_name("value")?))
}

struct Check<'s> {
    jest: &'s str,
    source: &'s str,
}

impl<'s> Check<'s> {
    fn block<'t>(&self, steps: impl Iterator<Item = Node<'t>>, mut aliases: HashSet<&'s str>) -> Option<Open> {
        let mut open = Open { falls_through: true, exits: false };
        for step in steps {
            if !open.falls_through {
                break;
            }
            let outcome = self.step(step, &mut aliases)?;
            open.exits |= outcome.exits;
            open.falls_through = outcome.falls_through;
        }
        Some(open)
    }

    fn step(&self, step: Node<'_>, aliases: &mut HashSet<&'s str>) -> Option<Open> {
        match step.kind() {
            "comment" | "empty_statement" => {}
            "return_statement" | "throw_statement" => {
                let restores = step.named_child(0).is_some_and(|value| self.restores(value, aliases));
                return Some(Open { falls_through: false, exits: !restores });
            }
            "statement_block" => return self.block(children(step), aliases.clone()),
            "if_statement" => {
                let yes = self.step(step.child_by_field_name("consequence")?, &mut aliases.clone())?;
                let no = match step.child_by_field_name("alternative") {
                    Some(clause) => self.step(clause.named_child(0)?, &mut aliases.clone())?,
                    None => Open { falls_through: true, exits: false },
                };
                return Some(yes.branch(no));
            }
            "try_statement" => return self.try_step(step, aliases),
            "expression_statement" if step.named_child(0).is_some_and(|value| self.restores(value, aliases)) => {
                return Some(Open::default());
            }
            "lexical_declaration" if const_binding(step, self.source).is_some() => {
                let (name, value) = const_binding(step, self.source)?;
                if aliases.contains(text(value, self.source)) {
                    aliases.insert(name);
                } else {
                    aliases.remove(name);
                    self.opaque(value, aliases)?;
                }
            }
            _ => self.opaque(step, aliases)?,
        }
        Some(Open { falls_through: true, exits: false })
    }

    fn try_step(&self, node: Node<'_>, aliases: &HashSet<&'s str>) -> Option<Open> {
        if let Some(finalizer) = node.child_by_field_name("finalizer") {
            let final_body = finalizer.child_by_field_name("body")?;
            if !self.block(children(final_body), aliases.clone())?.remains() {
                return Some(Open::default());
            }
        }
        // Catch and finally can run from intermediate states in the try.
        // Keep that possibility open unless the finalizer always restores.
        self.opaque(node, aliases)?;
        let exits = descendants(node).any(|part| part.kind() == "return_statement");
        Some(Open { falls_through: true, exits })
    }

    fn restores(&self, call: Node<'_>, aliases: &HashSet<&'s str>) -> bool {
        member_call(call, self.source).is_some_and(|(receiver, method)| {
            method == "mockRestore" && aliases.contains(receiver)
                || method == "restoreAllMocks" && receiver == self.jest
        })
    }

    fn opaque(&self, node: Node<'_>, aliases: &HashSet<&'s str>) -> Option<()> {
        (!descendants(node).any(|part| part.kind() == "identifier" && aliases.contains(text(part, self.source))))
            .then_some(())
    }
}
