use super::{Candidate, Hierarchy, Lookup, Members, Type, Visibility};

impl<'a> Lookup<'a> {
    pub fn resolve_static(
        hierarchy: &'a Hierarchy, owner: &'a Type, class: &str, name: &str, arity: usize,
    ) -> Option<Candidate<'a>> {
        let target = hierarchy.named(owner, class)?;
        if target.superclass.is_some() || !target.interfaces.is_empty() {
            return None;
        }
        let mut methods = target.methods.iter()
            .filter(|method| method.name == name)
            .map(|method| Candidate { owner: target, method })
            .filter(|candidate| candidate.accepts_arity(arity));
        let candidate = methods.next()?;
        (methods.next().is_none()
            && candidate.method.is_static
            && !candidate.method.varargs
            && !candidate.method.generic
            && match candidate.method.visibility {
                Visibility::Private => target.name == owner.name,
                Visibility::Package => target.package == owner.package,
                Visibility::Inherited => true,
            })
            .then_some(candidate)
    }

    pub fn instance_method_name(hierarchy: &'a Hierarchy, owner: &'a Type, name: &str) -> bool {
        let members = Members::load(hierarchy, owner, &owner.package);
        if members.missing {
            return false;
        }
        let mut methods = members.methods(name, Some(owner.name.as_str())).peekable();
        methods.peek().is_some() && methods.all(|candidate| !candidate.method.is_static)
    }
}
