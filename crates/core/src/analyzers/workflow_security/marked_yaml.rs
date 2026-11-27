use std::str::Chars;

use yaml_rust2::parser::{Event, Parser};

enum Value {
    Text(String),
    Mapping(Vec<(Node, Node)>),
    Sequence(Vec<Node>),
    Unsupported,
}

pub(super) struct Node {
    value: Value,
    pub(super) line: u32,
    pub(super) end_line: u32,
}

impl Node {
    pub(super) fn text(&self) -> Option<&str> {
        match &self.value {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    pub(super) fn fields(&self) -> &[(Node, Node)] {
        match &self.value {
            Value::Mapping(fields) => fields,
            _ => &[],
        }
    }

    pub(super) fn field(&self, name: &str) -> Option<(&Node, &Node)> {
        let mut matches = self
            .fields()
            .iter()
            .filter(|(key, _)| key.text() == Some(name));
        let (key, value) = matches.next()?;
        matches.next().is_none().then_some((key, value))
    }

    pub(super) fn sequence(&self) -> &[Node] {
        match &self.value {
            Value::Sequence(nodes) => nodes,
            _ => &[],
        }
    }
}

fn parse_node(parser: &mut Parser<Chars<'_>>, depth: usize) -> Option<Node> {
    if depth >= 64 {
        return None;
    }
    let (event, marker) = parser.next_token().ok()?;
    let value = match event {
        Event::Scalar(text, _, _, None) => Value::Text(text),
        Event::MappingStart(_, _) => {
            let mut fields = Vec::new();
            while parser.peek().ok()?.0 != Event::MappingEnd {
                fields.push((
                    parse_node(parser, depth + 1)?,
                    parse_node(parser, depth + 1)?,
                ));
            }
            parser.next_token().ok()?;
            Value::Mapping(fields)
        }
        Event::SequenceStart(_, _) => {
            let mut nodes = Vec::new();
            while parser.peek().ok()?.0 != Event::SequenceEnd {
                nodes.push(parse_node(parser, depth + 1)?);
            }
            parser.next_token().ok()?;
            Value::Sequence(nodes)
        }
        Event::Alias(_) | Event::Scalar(..) => Value::Unsupported,
        _ => return None,
    };
    let line = marker.line() as u32;
    let end_line = (parser.peek().ok()?.1.line() as u32)
        .saturating_sub(1)
        .max(line);
    Some(Node {
        value,
        line,
        end_line,
    })
}

pub(super) fn workflow(source: &str) -> Option<Node> {
    let mut parser = Parser::new_from_str(source);
    if parser.next_token().ok()?.0 != Event::StreamStart
        || parser.next_token().ok()?.0 != Event::DocumentStart
    {
        return None;
    }
    let root = parse_node(&mut parser, 0)?;
    (parser.next_token().ok()?.0 == Event::DocumentEnd
        && parser.next_token().ok()?.0 == Event::StreamEnd)
        .then_some(root)
}
