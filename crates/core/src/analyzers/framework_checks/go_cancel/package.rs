use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use autoreview_langsupport::{parser_for, Language};
use tree_sitter::{Node, Tree};

use super::super::{children, descendants, parse_source, text};
use super::flow::{Check, Helper, Helpers};
use super::two_result_call;

pub(super) struct File {
    pub path: String,
    pub package: String,
    pub source: String,
    tree: Tree,
    context: HashSet<String>,
}

pub(super) fn package_alias<'a>(file: &'a File, path: &str, default: &'a str) -> Option<&'a str> {
    descendants(file.root())
        .filter(|node| node.kind() == "import_spec")
        .find_map(|import| {
            let value = text(import.child_by_field_name("path")?, &file.source);
            if value.trim_matches(['"', '`']) != path {
                return None;
            }
            let alias = import
                .child_by_field_name("name")
                .map_or(default, |name| text(name, &file.source));
            (!matches!(alias, "." | "_")).then_some(alias)
        })
}

pub(super) fn load(root: &Path, directory: &Path) -> Vec<File> {
    let Ok(entries) = std::fs::read_dir(root.join(directory)) else {
        return Vec::new();
    };
    let Some(mut parser) = parser_for(Language::Go) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let absolute = entry.path();
        if absolute.extension().is_none_or(|ext| ext != "go") {
            continue;
        }
        let Some((source, tree)) = parse_source(&mut parser, &absolute) else {
            continue;
        };
        let Some(file) = file(root, &absolute, source, tree) else {
            continue;
        };
        files.push(file);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

fn file(root: &Path, absolute: &Path, source: String, tree: Tree) -> Option<File> {
    let node = tree.root_node();
    let package = children(node).find(|part| part.kind() == "package_clause")?;
    let context = descendants(node)
        .filter(|part| part.kind() == "import_spec")
        .filter(|import| {
            import
                .child_by_field_name("path")
                .is_some_and(|path| text(path, &source).trim_matches(['"', '`']) == "context")
        })
        .map(|import| {
            import
                .child_by_field_name("name")
                .map_or("context", |alias| text(alias, &source))
                .to_owned()
        })
        .collect();
    Some(File {
        path: absolute
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
        package: text(package.named_child(0)?, &source).to_owned(),
        source,
        tree,
        context,
    })
}

impl File {
    pub(super) fn functions(&self) -> impl Iterator<Item = Node<'_>> {
        children(self.tree.root_node())
            .filter(|item| matches!(item.kind(), "function_declaration" | "method_declaration"))
    }

    pub(super) fn root(&self) -> Node<'_> {
        self.tree.root_node()
    }

    fn factory(&self, call: Node<'_>, shadowed: &HashSet<&str>) -> bool {
        let Some(callee) = call
            .child_by_field_name("function")
            .filter(|part| part.kind() == "selector_expression")
        else {
            return false;
        };
        let package = callee
            .child_by_field_name("operand")
            .map(|node| text(node, &self.source));
        let name = callee
            .child_by_field_name("field")
            .map(|node| text(node, &self.source));
        package.is_some_and(|package| self.context.contains(package) && !shadowed.contains(package))
            && matches!(
                name,
                Some(
                    "WithCancel"
                        | "WithCancelCause"
                        | "WithTimeout"
                        | "WithTimeoutCause"
                        | "WithDeadline"
                        | "WithDeadlineCause"
                )
            )
    }

    fn cancel_parameter<'tree>(&self, function: Node<'tree>) -> Option<(usize, Node<'tree>)> {
        let mut position = 0;
        for declaration in children(function.child_by_field_name("parameters")?) {
            let ty = declaration.child_by_field_name("type")?;
            let names: Vec<_> = children(declaration)
                .filter(|node| node.kind() == "identifier")
                .collect();
            if names.len() == 1 && self.cancel_type(ty) {
                return Some((position, names[0]));
            }
            position += names.len().max(1);
        }
        None
    }

    fn cancel_type(&self, ty: Node<'_>) -> bool {
        let Some((package, name)) = text(ty, &self.source).rsplit_once('.') else {
            return false;
        };
        self.context.contains(package) && matches!(name, "CancelFunc" | "CancelCauseFunc")
    }

    fn forwards_factory(&self, function: Node<'_>) -> bool {
        let Some(body) = function.child_by_field_name("body") else {
            return false;
        };
        let Some(list) = children(body).find(|node| node.kind() == "statement_list") else {
            return false;
        };
        let mut statements = children(list).filter(|node| node.kind() != "comment");
        let Some(ret) = statements
            .next()
            .filter(|node| node.kind() == "return_statement")
        else {
            return false;
        };
        statements.next().is_none()
            && ret
                .named_child(0)
                .and_then(|list| list.named_child(0))
                .is_some_and(|call| {
                    call.kind() == "call_expression"
                        && self.factory(call, &local_names(function, &self.source))
                })
    }
}

pub(super) struct Summaries<'a> {
    pub helpers: Helpers,
    wrappers: HashSet<String>,
    helper_files: HashMap<String, (&'a File, Node<'a>)>,
}

impl<'a> Summaries<'a> {
    pub fn evidence(&self, names: Vec<String>) -> impl Iterator<Item = (&'a File, Node<'a>)> + '_ {
        let mut seen = BTreeSet::new();
        let mut pending = names;
        while let Some(name) = pending.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(helper) = self.helpers.get(&name) {
                pending.extend(helper.calls.iter().cloned());
            }
        }
        seen.into_iter()
            .filter_map(|name| self.helper_files.get(&name).copied())
    }
}

pub(super) fn summaries<'a>(files: &[&'a File]) -> Summaries<'a> {
    let mut sites = HashMap::new();
    let mut ambiguous = HashSet::new();
    for &file in files {
        for function in file
            .functions()
            .filter(|node| node.kind() == "function_declaration")
        {
            let Some(name) = function.child_by_field_name("name") else {
                continue;
            };
            let name = text(name, &file.source).to_owned();
            if sites.insert(name.clone(), (file, function)).is_some() {
                ambiguous.insert(name);
            }
        }
    }
    sites.retain(|name, _| !ambiguous.contains(name));
    let wrappers = sites
        .iter()
        .filter(|(_, (file, function))| file.forwards_factory(*function))
        .map(|(name, _)| name.clone())
        .collect();
    let mut helpers = Helpers::new();
    while advance(&sites, &mut helpers) {}
    Summaries {
        helpers,
        wrappers,
        helper_files: sites,
    }
}

fn advance(sites: &HashMap<String, (&File, Node<'_>)>, helpers: &mut Helpers) -> bool {
    let before = helpers.len();
    for (name, (file, function)) in sites {
        if helpers.contains_key(name) {
            continue;
        }
        let Some((index, binding)) = file.cancel_parameter(*function) else {
            continue;
        };
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        let shadowed = local_names(*function, &file.source);
        let mut check = Check::new(
            &file.source,
            text(binding, &file.source),
            None,
            helpers,
            &shadowed,
        );
        if let Some(consumes) = check.check(body) {
            let calls = std::mem::take(&mut check.used);
            helpers.insert(
                name.clone(),
                Helper {
                    position: index,
                    consumes,
                    calls,
                },
            );
        }
    }
    before != helpers.len()
}

pub(super) fn leaks<'a>(
    file: &'a File,
    summaries: &Summaries<'_>,
) -> Vec<(Node<'a>, Node<'a>, Vec<String>)> {
    let mut leaks = Vec::new();
    for function in file.functions() {
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        let shadowed = local_names(function, &file.source);
        for assignment in descendants(body).filter(|node| node.kind() == "short_var_declaration") {
            let Some((binding, wrapper)) = acquisition(file, assignment, summaries, &shadowed)
            else {
                continue;
            };
            let mut check = Check::new(
                &file.source,
                text(binding, &file.source),
                Some(assignment),
                &summaries.helpers,
                &shadowed,
            );
            if check.check(body) == Some(false) {
                check.used.extend(wrapper.map(str::to_owned));
                if let Some(exit) = check.exit {
                    leaks.push((binding, exit, check.used));
                }
            }
        }
    }
    leaks
}

fn acquisition<'tree, 'src>(
    file: &'src File,
    assignment: Node<'tree>,
    summaries: &Summaries<'_>,
    shadowed: &HashSet<&str>,
) -> Option<(Node<'tree>, Option<&'src str>)> {
    let (binding, call) = two_result_call(assignment)?;
    if text(binding, &file.source) == "_" {
        return None;
    }
    let callee = call.child_by_field_name("function")?;
    if file.factory(call, shadowed) {
        return Some((binding, None));
    }
    let name = text(callee, &file.source);
    (callee.kind() == "identifier" && !shadowed.contains(name) && summaries.wrappers.contains(name))
        .then_some((binding, Some(name)))
}

pub(super) fn local_names<'src>(function: Node<'_>, source: &'src str) -> HashSet<&'src str> {
    descendants(function)
        .flat_map(|node| {
            match node.kind() {
                "short_var_declaration" | "range_clause" => node.child_by_field_name("left"),
                "type_switch_statement" => node.child_by_field_name("alias"),
                "parameter_declaration"
                | "variadic_parameter_declaration"
                | "var_spec"
                | "const_spec" => Some(node),
                _ => None,
            }
            .into_iter()
            .flat_map(children)
            .filter(|child| child.kind() == "identifier")
            .map(|child| text(child, source))
        })
        .collect()
}
