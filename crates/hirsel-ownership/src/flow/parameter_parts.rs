//! Finite observations of parameter field paths for the frame's result relation.
//! A matched prefix stays matched under further projection; a divergent prefix
//! stays outside. Only prefixes still capable of matching need distinct states.
use super::*;

const WITHIN: usize = 0;
const OUTSIDE: usize = 1;

pub(super) struct ParameterParts {
    roots: BTreeMap<usize, usize>,
    next: Vec<BTreeMap<u32, usize>>,
}

impl ParameterParts {
    pub fn new(body: &CheckedBody, graph: &Graph, rows: &crate::source_rows::SourceRows) -> Self {
        let mut parts = Self {
            roots: BTreeMap::new(),
            next: vec![BTreeMap::new(), BTreeMap::new()],
        };
        for reach in graph.result.relation.reaches() {
            let Reach::Place(Origin {
                root: OriginRoot::Parameter(parameter),
                fields,
            }) = reach
            else {
                continue;
            };
            let (scope, parameter) = match parameter {
                ParameterRef::Bound {
                    depth: 0,
                    parameter,
                } => (graph.owner, *parameter),
                ParameterRef::Body { scope, parameter } if scope.owner == body.lexical => {
                    (scope.site, *parameter)
                }
                _ => continue,
            };
            let Some(root) = rows.binding(body, Some(scope), &ResolvedName::Parameter(parameter))
            else {
                continue;
            };
            parts.insert(root, fields.iter().map(|field| field.0));
        }
        parts
    }

    fn node(&mut self) -> usize {
        let id = self.next.len();
        self.next.push(BTreeMap::new());
        id
    }

    fn insert(&mut self, root: usize, fields: impl IntoIterator<Item = u32>) {
        let start = if let Some(&start) = self.roots.get(&root) {
            start
        } else {
            let start = self.node();
            self.roots.insert(root, start);
            start
        };
        let mut at = start;
        // The final edge enters the absorbing matched state. A shorter declared
        // prefix subsumes longer declarations, in either insertion order.
        let fields: Vec<_> = fields.into_iter().collect();
        if fields.is_empty() {
            self.roots.insert(root, WITHIN);
        }
        for (index, field) in fields.iter().enumerate() {
            if at == WITHIN {
                break;
            }
            if index + 1 == fields.len() {
                self.next[at].insert(*field, WITHIN);
            } else if let Some(&child) = self.next[at].get(field) {
                at = child;
            } else {
                let child = self.node();
                self.next[at].insert(*field, child);
                at = child;
            }
        }
    }

    pub fn start(&self, root: usize) -> Option<usize> {
        self.roots.get(&root).copied()
    }

    pub fn project(&self, mut state: usize, fields: impl IntoIterator<Item = u32>) -> usize {
        for field in fields {
            if state == WITHIN || state == OUTSIDE {
                break;
            }
            state = self.next[state].get(&field).copied().unwrap_or(OUTSIDE);
        }
        state
    }

    pub fn outside(&self, value: &Value) -> bool {
        let reached = value.reached();
        self.roots.iter().any(|(root, start)| {
            *start != WITHIN
                && reached.contains(root)
                && !matches!(&value.part_of, Some((source, states))
                    if source == root && states.iter().all(|state| *state == WITHIN))
        })
    }
}
