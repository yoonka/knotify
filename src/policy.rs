//! Soft path rules never override the native hard exclusions in Catalog.
use crate::wire::Value;

#[derive(Default)]
pub struct Policy {
    rules: Vec<(Vec<u8>, bool)>,
    names: Vec<Vec<u8>>,
}
pub fn within(path: &[u8], prefix: &[u8]) -> bool {
    prefix.is_empty()
        || path == prefix
        || (path.starts_with(prefix) && path.get(prefix.len()) == Some(&b'/'))
}
impl Policy {
    pub fn parse(value: Option<&Value>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let mut rules = vec![];
        for rule in value.get("rules")?.list()? {
            let path = rule.get("path")?.bytes()?.to_vec();
            if !path.is_empty()
                && path
                    .split(|b| *b == b'/')
                    .any(|p| p.is_empty() || p == b"." || p == b".." || p.contains(&0))
            {
                return Err("invalid policy path".into());
            }
            let excluded = match rule.get("exclude")? {
                Value::Bool(v) => *v,
                Value::Nil => continue,
                _ => return Err("invalid policy exclusion".into()),
            };
            if rules.iter().any(|(p, _)| p == &path) {
                return Err("duplicate policy path".into());
            }
            rules.push((path, excluded));
        }
        let names = value
            .get("default_exclusions")?
            .list()?
            .iter()
            .map(|v| v.bytes().map(|b| b.to_vec()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { rules, names })
    }
    pub fn allows(&self, path: &[u8]) -> bool {
        if let Some((_, excluded)) = self
            .rules
            .iter()
            .filter(|(prefix, _)| within(path, prefix))
            .max_by_key(|(prefix, _)| prefix.len())
        {
            return !excluded;
        }
        !path.split(|b| *b == b'/').any(|name| {
            self.names.iter().any(|pattern| {
                pattern
                    .strip_suffix(b"*")
                    .map_or(name == pattern, |prefix| name.starts_with(prefix))
            })
        })
    }
    pub fn traverse(&self, path: &[u8]) -> bool {
        self.allows(path)
            || self
                .rules
                .iter()
                .any(|(prefix, excluded)| !excluded && within(prefix, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_ancestor_wins_and_nested_inclusions_are_reachable() {
        let p = Policy {
            rules: vec![
                (b"private".to_vec(), true),
                (b"private/public".to_vec(), false),
            ],
            names: vec![b".Trash-*".to_vec()],
        };
        assert!(!p.allows(b"private/a"));
        assert!(p.traverse(b"private"));
        assert!(p.allows(b"private/public/a"));
        assert!(p.allows(b"private-copy/a"));
        assert!(!p.allows(b"x/.Trash-100/a"));
        assert!(!p.traverse(b"x/.Trash-100"));
    }
}
